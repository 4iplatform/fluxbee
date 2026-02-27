# JSON Router - 07 Operaciones

**Estado:** v1.16  
**Fecha:** 2026-02-04  
**Audiencia:** Ops/SRE, desarrolladores de deployment

---

## 1. Filosofía de Configuración

### 1.1 Principio

El usuario configura **solo** lo que depende de su infraestructura. El sistema maneja todo lo demás con defaults hardcodeados.

| Configura el usuario | Hardcodeado en el sistema |
|---------------------|---------------------------|
| `hive_id` | Paths de directorios |
| `wan.gateway_name` (opcional) | Qué nodos SY arrancan |
| `wan.listen` (IP:puerto) | Timers internos |
| `wan.uplinks[]` | Límites (MAX_NODES, etc.) |
| `admin.listen` (opcional) | Orden de arranque |
| `blob.*` (opcional) | Implementación interna de sync/tooling |

### 1.2 Paths Fijos (No Configurables)

Todos los binarios conocen estos paths por código:

```
/etc/fluxbee/                  # Configuración (solo hive.yaml lo toca el humano)
└── hive.yaml                    # Identidad y WAN (ÚNICO archivo que edita el humano)

/var/lib/fluxbee/              # Estado persistente (auto-generado, persistido por SY.*)
├── identity.yaml                  # UUID del gateway (auto-generado)
├── orchestrator.yaml              # Estado interno de SY.orchestrator (no fuente de config)
├── config-routes.yaml             # Rutas/VPN (persiste SY.config.routes)
├── opa-version.txt                # Contador monotónico de versión OPA (SY.admin)
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
├── syncthing/                     # Estado de Syncthing (solo si blob.sync.enabled=true)
├── nodes/                         # UUIDs de nodos
│   └── AI.soporte.l1.uuid
└── hives/                       # Repo de islas hijas (solo en mother)
    └── staging/
        ├── ssh.key
        ├── ssh.key.pub
        └── info.yaml

/var/run/fluxbee/              # Runtime (volátil)
├── routers/
│   └── <router-uuid>.sock
└── orchestrator.pid

/dev/shm/                          # Shared memory
├── jsr-<router-uuid>
├── jsr-config-<hive>
├── jsr-lsa-<hive>
├── jsr-opa-<hive>               # WASM de policy OPA
└── jsr-identity-<hive>          # Identity table (ILKs, degrees, modules)
```

---

## 2. Archivo hive.yaml

El **único** archivo que el usuario crea/edita.

### 2.1 Ejemplo Mínimo (isla standalone)

```yaml
# /etc/fluxbee/hive.yaml
hive_id: dev
```

Con esto el sistema levanta una isla funcional sin conexión WAN (usa SQLite embebido para contextos).

### 2.2 Ejemplo Motherbee (isla madre)

```yaml
# /etc/fluxbee/hive.yaml
hive_id: produccion
role: motherbee

wan:
  gateway_name: RT.gateway         # Opcional, default: RT.gateway
  listen: "0.0.0.0:9000"           # Escuchar conexiones de workers

nats:
  mode: embedded
  port: 4222

blob:
  enabled: true
  path: "/var/lib/fluxbee/blob"
  sync:
    enabled: false

database:
  url: "postgresql://fluxbee:password@localhost:5432/fluxbee"
  pool_size: 10
```

### 2.3 Ejemplo Worker (isla hija)

```yaml
# /etc/fluxbee/hive.yaml (generado por add_hive o manual)
hive_id: staging
role: worker

wan:
  gateway_name: RT.gateway
  uplinks:
    - address: "192.168.1.10:9000"  # Motherbee

nats:
  mode: embedded
  port: 4222

blob:
  enabled: true
  path: "/var/lib/fluxbee/blob"
  sync:
    enabled: false
```

### 2.4 Campos de hive.yaml

| Campo | Obligatorio | Default | Descripción |
|-------|-------------|---------|-------------|
| `hive_id` | **Sí** | - | Identificador único de la isla |
| `role` | No | `worker` | `motherbee` o `worker` |
| `wan.gateway_name` | No | `RT.gateway` | Nombre del router gateway |
| `wan.listen` | No | (sin escucha) | IP:puerto para recibir conexiones WAN |
| `wan.uplinks[]` | No | [] | Lista de gateways a conectar (workers) |
| `nats.mode` | No | `embedded` | `embedded` o `client` |
| `nats.port` | No | 4222 | Puerto NATS si embedded |
| `nats.url` | No | - | URL si mode=client |
| `storage.path` | No | `/var/lib/fluxbee` | Root de storage compartido de módulos/artefactos |
| `blob.enabled` | No | `true` | Habilita capa blob local |
| `blob.path` | No | `/var/lib/fluxbee/blob` | Path base de blobs |
| `blob.sync.enabled` | No | `false` | Activa sincronización externa de blobs (gestionada por orchestrator) |
| `blob.sync.tool` | No | `syncthing` | Herramienta de sincronización (actual: Syncthing) |
| `blob.sync.api_port` | No | 8384 | API local de Syncthing |
| `blob.sync.data_dir` | No | `/var/lib/fluxbee/syncthing` | Directorio de estado de Syncthing |
| `admin.listen` | No | `127.0.0.1:8080` | Bind del API HTTP de SY.admin (recomendado loopback + proxy) |
| `database.url` | Solo Motherbee | - | Connection string PostgreSQL |
| `database.pool_size` | No | 10 | Conexiones en el pool |

### 2.5 Blob Sync (Syncthing) - Operación

Regla de activación:
1. Se habilita desde `hive.yaml` de Motherbee (`blob.sync.enabled=true`).
2. `SY.orchestrator` aplica setup local y propaga setup a hives gestionadas.
3. En `add_hive`, la isla nueva ya queda con Syncthing instalado/configurado si sync está activo.

Lifecycle gestionado por orchestrator:
- Unit systemd: `fluxbee-syncthing.service`.
- Health local: conexión TCP a `127.0.0.1:<blob.sync.api_port>`.
- Watchdog: restart automático si servicio/API no está sano.
- Reconciliación por config: si `blob.sync.enabled` pasa a `false` (o se quita en `hive.yaml` de Motherbee), el orchestrator revierte setup local/remoto:
  - `systemctl stop/disable fluxbee-syncthing`,
  - remueve unit `fluxbee-syncthing.service`,
  - ejecuta `daemon-reload`,
  - elimina reglas de firewall Syncthing en hosts gestionados.

Puertos operativos Syncthing:
- `22000/tcp` (sync)
- `22000/udp` (QUIC/sync)
- `21027/udp` (local discovery)
- `8384/tcp` (API GUI local; por default enlazada a `127.0.0.1`)

Firewall:
- El orchestrator intenta abrir puertos con `ufw` o `firewalld` (local y remoto).
- Si no detecta `ufw/firewalld`, deja warning en logs y la apertura queda a cargo de la política de host.

### 2.6 Exposición del API Admin (perfil seguro)

Política recomendada:
1. Mantener `admin.listen` en loopback (`127.0.0.1:8080`) y publicar externamente solo vía proxy.
2. Si se requiere bind directo (`0.0.0.0`), restringir por firewall/ACL a rangos de administración.
3. Exigir autenticación/autorización en el proxy (mTLS o auth de red interna).
4. Registrar auditoría de accesos al API (`POST/DELETE /hives`, `PUT /config/*`, OPA).

Comportamiento actual:
- Si `admin.listen` no está definido en `hive.yaml`, `SY.admin` usa `127.0.0.1:8080`.
- Override operativo opcional: variable de entorno `JSR_ADMIN_LISTEN`.

### 2.7 Break-Glass SSH (acceso de emergencia)

Objetivo:
- Recuperar control de un worker si el acceso remoto de orchestrator queda degradado por restricciones de `authorized_keys`/gate.

Precondiciones:
- Acceso administrativo a motherbee (host donde corre `SY.orchestrator`).
- Acceso out-of-band al worker (consola/hipervisor) para el peor caso.

Escenario A: hay key, pero la restricción bloquea operaciones
1. Desactivar temporalmente restricciones en motherbee:
```bash
sudo mkdir -p /etc/systemd/system/sy-orchestrator.service.d
cat <<'EOF' | sudo tee /etc/systemd/system/sy-orchestrator.service.d/90-break-glass.conf
[Service]
Environment=ORCH_AUTHKEY_ENFORCE_GATE=0
Environment=ORCH_AUTHKEY_ENFORCE_FROM=0
EOF
sudo systemctl daemon-reload
sudo systemctl restart sy-orchestrator
```
2. Rebootstrap del worker (reinstala `authorized_keys`/sudoers):
```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="worker-220"
HIVE_ADDR="192.168.8.220"
curl -sS -X DELETE "$BASE/hives/$HIVE_ID"; echo
curl -sS -X POST "$BASE/hives" -H "Content-Type: application/json" \
  -d "{\"hive_id\":\"$HIVE_ID\",\"address\":\"$HIVE_ADDR\"}"; echo
```
3. Volver a modo seguro por defecto:
```bash
sudo rm -f /etc/systemd/system/sy-orchestrator.service.d/90-break-glass.conf
sudo systemctl daemon-reload
sudo systemctl restart sy-orchestrator
```

Escenario B (pre-S5): key caída y password aún habilitado
- Usar login por password solo para recuperación inicial, luego repetir Escenario A.
- No dejar password en scripts/historial; rotar secreto tras recuperación.

Escenario C (post-S5): key caída y password deshabilitado
- Recuperar por consola out-of-band y reinstalar manualmente la key pública de motherbee:
```bash
sudo install -d -m 700 -o administrator -g administrator /home/administrator/.ssh
sudo sh -lc 'cat >> /home/administrator/.ssh/authorized_keys' <<'EOF'
<MOTHERBEE_PUBLIC_KEY_LINE>
EOF
sudo chown administrator:administrator /home/administrator/.ssh/authorized_keys
sudo chmod 600 /home/administrator/.ssh/authorized_keys
```
- Luego ejecutar `DELETE/POST /hives/{id}` desde motherbee para reconciliar estado.

Evidencia mínima de recuperación:
- `GET /hives/<id>` responde `status=ok`.
- `GET /versions?hive=<id>` devuelve `core/runtime/vendor`.
- `GET /deployments?hive=<id>&limit=20` contiene entradas recientes de reconciliación.

---

## 3. Distribución Motherbee / Worker

### 3.1 Modelo de Deployment

```
┌─────────────────────────────────────────────────────────────────┐
│                         MOTHERBEE                               │
│                    (isla madre, cerebro)                        │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Componentes exclusivos de Motherbee:                    │   │
│  │  • PostgreSQL (source of truth)                         │   │
│  │  • SY.storage (único que escribe DB)                    │   │
│  │  • SY.orchestrator (supervisa todo)                     │   │
│  │  • SY.admin (API admin, comandos)                       │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Componentes compartidos (también en Motherbee):         │   │
│  │  • Router + NATS embebido                               │   │
│  │  • SY.identity, SY.config.routes, SY.opa.rules         │   │
│  │  • SY.cognition + LanceDB + jsr-memory                 │   │
│  │  • Nodos AI/IO/WF (opcional)                           │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ WAN (TCP)
          ┌───────────────────┼───────────────────┐
          │                   │                   │
          ▼                   ▼                   ▼
┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐
│     WORKER      │  │     WORKER      │  │     WORKER      │
│   (isla hija)   │  │   (isla hija)   │  │   (isla hija)   │
│                 │  │                 │  │                 │
│ • Router + NATS │  │ • Router + NATS │  │ • Router + NATS │
│ • SY.identity*  │  │ • SY.identity*  │  │ • SY.identity*  │
│ • SY.cognition  │  │ • SY.cognition  │  │ • SY.cognition  │
│ • LanceDB       │  │ • LanceDB       │  │ • LanceDB       │
│ • AI/IO/WF      │  │ • AI/IO/WF      │  │ • AI/IO/WF      │
│                 │  │                 │  │                 │
│ *cache de madre │  │ *cache de madre │  │ *cache de madre │
└─────────────────┘  └─────────────────┘  └─────────────────┘
    Stateless           Stateless           Stateless
    (reconstruible)     (reconstruible)     (reconstruible)
```

### 3.2 Tabla de Componentes por Rol

| Componente | Motherbee | Worker | Notas |
|------------|:---------:|:------:|-------|
| **Infraestructura** |
| PostgreSQL | ✓ | - | Source of truth |
| SY.storage | ✓ | - | Único que escribe DB |
| SY.orchestrator | ✓ | - | Supervisa todo el cluster |
| SY.admin | ✓ | - | API admin HTTP |
| **Router** |
| Router | ✓ | ✓ | |
| NATS embebido | ✓ | ✓ | Buffer local |
| WAN bridge | ✓ | ✓ | Si config.wan presente |
| Syncthing (opcional) | ✓ | ✓ | Solo si `blob.sync.enabled=true`; orchestrator instala/arranca/monitorea local+remoto |
| **Sistema** |
| SY.identity | ✓ | ✓ (cache) | Worker sincroniza de Motherbee |
| SY.config.routes | ✓ | ✓ (cache) | Worker sincroniza de Motherbee |
| SY.opa.rules | ✓ | ✓ (cache) | Worker sincroniza de Motherbee |
| **Cognición** |
| SY.cognition | ✓ | ✓ | Procesa local |
| LanceDB | ✓ | ✓ | Cache reconstruible |
| jsr-memory | ✓ | ✓ | Índice local |
| **Aplicación** |
| AI.* | opcional | ✓ | Nodos de aplicación |
| IO.* | opcional | ✓ | Conectores externos |
| WF.* | opcional | ✓ | Workflows |

### 3.3 Workers son Stateless

Los workers pueden destruirse y recrearse sin pérdida de datos:

```
Worker muere/se destruye:
├── NATS local tenía buffer → perdido (pero ya estaba en Motherbee o en tránsito)
├── LanceDB local → se reconstruye desde PostgreSQL (via SY.storage)
├── jsr-memory → se regenera desde LanceDB
└── Nodos AI/IO/WF → se reinician, sin estado

Worker nuevo arranca:
├── Conecta a Motherbee (WAN)
├── SY.cognition hace cold start (rebuild desde PostgreSQL)
├── SY.identity/config/opa sincronizan de Motherbee
└── Listo para recibir trabajo
```

Esto hace que los workers sean ideales para containers (Docker, K8s).

---

## 4. SY.orchestrator: Supervisor del Cluster

### 4.1 Ubicación

**SY.orchestrator SOLO corre en Motherbee.** Los workers no tienen orchestrator propio; systemd local basta para mantener el router vivo.

### 4.2 Rol

SY.orchestrator es el **único proceso que se inicia manualmente** en Motherbee. Él:

1. **Levanta** todos los componentes de Motherbee
2. **Monitorea** heartbeats en SHM (watchdog)
3. **Gestiona** ciclo de vida de nodos de aplicación
4. **Ejecuta** bootstrap de workers remotos (add_hive)
5. **Supervisa** salud de NATS (buffer levels)
6. **Gestiona** Syncthing para blob sync (si `blob.sync.enabled=true`)

### 4.3 Métodos de Supervisión

| Componente | Método de inicio | Supervisión | Si muere |
|------------|------------------|-------------|----------|
| RT.gateway | systemd | SHM heartbeat | `systemctl restart` |
| SY.storage | systemd | SHM heartbeat | `systemctl restart` |
| SY.identity | systemd | SHM heartbeat | `systemctl restart` |
| SY.config.routes | systemd | SHM heartbeat | `systemctl restart` |
| SY.opa.rules | systemd | SHM heartbeat | `systemctl restart` |
| SY.admin | systemd | SHM heartbeat | `systemctl restart` |
| SY.cognition | systemd | SHM heartbeat | `systemctl restart` |
| fluxbee-syncthing (opcional) | systemd | API local `127.0.0.1:<api_port>` | `systemctl restart` |
| AI.* / IO.* / WF.* | exec/spawn | Proceso hijo | Log warning, respawn opcional |

**Core via systemd:** Máxima estabilidad, el kernel reinicia si falla.  
**App via exec:** Agilidad, control directo, fácil escalar.

### 4.4 Watchdog

El orchestrator verifica cada 5 segundos:

```rust
impl Orchestrator {
    async fn watchdog_loop(&mut self) {
        loop {
            // 1. Verificar heartbeats en SHM
            for component in &self.core_components {
                let shm = self.read_shm(component);
                
                if shm.heartbeat_stale() {
                    log::error!("{} heartbeat stale, restarting", component);
                    self.restart_via_systemd(component).await;
                }
            }
            
            // 2. Verificar NATS health
            if let Some(nats_stats) = self.get_nats_stats() {
                if nats_stats.free_bytes < NATS_LOW_MEMORY_THRESHOLD {
                    log::warn!("NATS buffer low: {} bytes free", nats_stats.free_bytes);
                    // Podría: alertar, purgar mensajes viejos, etc.
                }
            }
            
            // 3. Verificar procesos app (spawn)
            for (name, child) in &mut self.app_processes {
                if child.try_wait().is_some() {
                    log::warn!("{} exited", name);
                    // Opcionalmente respawnear según config
                }
            }
            
            sleep(Duration::from_secs(5)).await;
        }
    }
}
```

### 4.5 Arranque de Motherbee

```bash
# Esto es todo lo que ejecuta el operador:
systemctl start sy-orchestrator
```

### 4.6 Secuencia de Bootstrap

```
┌──────────────────────────────────────────────────────────────┐
│ FASE 0: Inicialización                                       │
├──────────────────────────────────────────────────────────────┤
│ 1. Leer /etc/fluxbee/hive.yaml                            │
│ 2. Verificar role: motherbee                                │
│ 3. Crear directorios si no existen                          │
│ 4. Escribir PID en /var/run/fluxbee/orchestrator.pid        │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 1: PostgreSQL y Storage                                 │
├──────────────────────────────────────────────────────────────┤
│ 1. Verificar PostgreSQL disponible                          │
│ 2. systemctl start sy-storage                               │
│ 3. Esperar que SY.storage conecte (30s timeout)             │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 2: Router Gateway                                       │
├──────────────────────────────────────────────────────────────┤
│ 1. systemctl start rt-gateway                               │
│ 2. Esperar socket disponible                                │
│ 3. Esperar región SHM jsr-<uuid> creada                     │
│ 4. Esperar NATS embebido listo                              │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 3: Nodos SY de Sistema                                  │
├──────────────────────────────────────────────────────────────┤
│ En paralelo:                                                 │
│ 1. systemctl start sy-identity     → jsr-identity           │
│ 2. systemctl start sy-config       → jsr-config             │
│ 3. systemctl start sy-opa          → jsr-opa                │
│ 4. systemctl start sy-cognition    → jsr-memory + LanceDB   │
│ 5. systemctl start sy-admin        → API HTTP               │
│ 6. Esperar que todos conecten al router (30s timeout)       │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 4: Orchestrator se conecta                              │
├──────────────────────────────────────────────────────────────┤
│ 1. Conectar al router como nodo SY.orchestrator@<isla>      │
│ 2. HELLO → ANNOUNCE                                          │
│ 3. Ahora puede enviar/recibir mensajes                      │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 5: Motherbee Operativa                                  │
├──────────────────────────────────────────────────────────────┤
│ 1. Log: "Motherbee {hive_id} ready"                       │
│ 2. Entrar en loop principal:                                │
│    • Watchdog: verificar heartbeats y NATS                  │
│    • Procesar mensajes (add_hive, run_node, etc.)         │
│    • Reiniciar componentes caídos                           │
└──────────────────────────────────────────────────────────────┘
```

### 4.7 Arranque de Worker

Los workers son más simples. Solo necesitan systemd local:

```bash
# En el worker (o via add_hive desde Motherbee)
systemctl start fluxbee-worker
```

```
┌──────────────────────────────────────────────────────────────┐
│ Worker Bootstrap                                             │
├──────────────────────────────────────────────────────────────┤
│ 1. Leer /etc/fluxbee/hive.yaml (role: worker)            │
│ 2. Iniciar router + NATS embebido                           │
│ 3. Conectar WAN a Motherbee                                 │
│ 4. Iniciar SY.cognition (cold start → rebuild desde PG)    │
│ 5. Sincronizar identity/config/opa de Motherbee            │
│ 6. Iniciar nodos AI/IO/WF según config                     │
│ 7. Log: "Worker {hive_id} ready"                          │
└──────────────────────────────────────────────────────────────┘
```

### 4.8 Shutdown

**Motherbee:**
```bash
systemctl stop sy-orchestrator
```

El orchestrator hace shutdown ordenado:
1. Envía SIGTERM a todos los nodos AI/WF/IO que levantó
2. Espera 10s
3. Detiene SY.* via systemctl stop
4. Detiene RT.gateway
5. Sale

**Worker:**
```bash
systemctl stop fluxbee-worker
```

Más simple: detiene router (que detiene NATS), los nodos se desconectan.

### 4.9 Gestión de Runtimes

El orchestrator mantiene sincronizados los binarios ejecutables (runtimes) en todos los workers.

#### 4.9.1 Modelo

```
┌─────────────────────────────────────────────────────────────────┐
│                        MOTHERBEE                                │
│                                                                 │
│  /var/lib/fluxbee/runtimes/         ← REPO MASTER              │
│  ├── AI.soporte/                                                │
│  │   ├── 1.2.0/                                                │
│  │   └── 1.3.0/                                                │
│  ├── IO.whatsapp/                                               │
│  └── manifest.json                  ← Estado actual            │
│                                                                 │
│  SY.orchestrator                                                │
│  ├── Recibe notificaciones de nuevas versiones                 │
│  ├── Persiste manifest en /var/lib/fluxbee/orchestrator/       │
│  ├── Sincroniza workers via SSH/rsync                          │
│  └── Verifica periódicamente consistencia                      │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ SSH (sync)
                              ▼
                          WORKERS
```

#### 4.9.2 Notificación de Nueva Versión

El orchestrator recibe un mensaje JSON (via router) cuando hay nuevas versiones:

```json
{
  "routing": {
    "src": "<quien-sea>",
    "dst": "SY.orchestrator@motherbee"
  },
  "meta": {
    "type": "system",
    "msg": "RUNTIME_UPDATE"
  },
  "payload": {
    "version": 43,
    "updated_at": "2026-02-08T10:00:00Z",
    "runtimes": {
      "AI.soporte": {
        "current": "1.3.0",
        "available": ["1.2.0", "1.3.0"]
      },
      "IO.whatsapp": {
        "current": "2.1.0",
        "available": ["2.0.0", "2.1.0"]
      }
    },
    "hash": "sha256:abc123..."
  }
}
```

#### 4.9.3 Persistencia Local

El orchestrator guarda el manifest para sí mismo:

```
/var/lib/fluxbee/orchestrator/
└── runtime-manifest.json    ← Copia local, solo orchestrator lee/escribe
```

#### 4.9.4 Flujo de Sincronización

```
1. Orchestrator recibe RUNTIME_UPDATE
        │
        ▼
2. Compara con manifest actual
        │
        ├── Sin cambios → ignorar
        │
        └── Hay cambios:
                │
                ▼
3. Persiste nuevo manifest local
        │
        ▼
4. Para cada worker:
        │
        ├── rsync /var/lib/fluxbee/runtimes/ → worker
        │
        └── Reinicia nodos afectados (si corrían versión vieja)
        │
        ▼
5. Log: "Runtimes synced to version {version}"
```

#### 4.9.5 Verificación Periódica

Cada 5 minutos, el orchestrator verifica que los workers estén en sync:

```rust
impl Orchestrator {
    async fn runtime_verify_loop(&mut self) {
        loop {
            for worker in &self.workers {
                // Comparar hash del manifest remoto vs local
                let remote_hash = self.ssh_exec(
                    worker,
                    "sha256sum /var/lib/fluxbee/runtimes/manifest.json"
                ).await;
                
                if remote_hash != self.local_manifest_hash {
                    log::warn!("Worker {} drift detected, syncing", worker.id);
                    self.sync_worker(worker).await;
                }
            }
            
            sleep(Duration::from_secs(300)).await;  // 5 min
        }
    }
}
```

#### 4.9.6 Spawn de Nodos

Cuando se pide ejecutar un nodo:

```json
{
  "routing": {
    "src": "SY.admin@motherbee",
    "dst": "SY.orchestrator@motherbee"
  },
  "meta": {
    "type": "system",
    "msg": "SPAWN_NODE"
  },
  "payload": {
    "runtime": "AI.soporte",
    "version": "1.3.0",        // Opcional, default = current
    "target": "worker-3",       // Opcional, orchestrator decide si no se especifica
    "config": { ... }
  }
}
```

El orchestrator:
1. Verifica que el runtime exista en el manifest
2. Verifica que el worker tenga el runtime (o lo sincroniza)
3. Ejecuta via SSH: `/var/lib/fluxbee/runtimes/AI.soporte/1.3.0/bin/start.sh`

#### 4.9.7 Constantes

```rust
pub const RUNTIME_VERIFY_INTERVAL_SECS: u64 = 300;  // 5 minutos
pub const RUNTIME_SYNC_TIMEOUT_SECS: u64 = 300;     // 5 minutos max
```

---

## 5. Bootstrap de Workers Remotos (add_hive)

Esta funcionalidad permite instalar y configurar workers remotos automáticamente desde Motherbee.

### 5.1 Escenario

```
┌─────────────────────────────────────────────────────────────────┐
│                         INTERNET                                │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│                        MOTHERBEE                                │
│                    (tiene internet)                             │
│                  hive_id: produccion                          │
│                                                                 │
│   SY.orchestrator ← ejecuta add_hive                         │
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
    │ (worker1)│         │ (worker2)│         │ (worker3)│
    └──────────┘         └──────────┘         └──────────┘
    
    Solo tienen: Linux + SSH (port 22) + user administrator
```

### 5.2 Requisitos de la Máquina Nueva

| Requisito | Valor | Notas |
|-----------|-------|-------|
| OS | Linux (cualquier distro con systemd) | Ubuntu, Debian, RHEL, etc. |
| SSH | Puerto 22, habilitado | Viene por defecto en la mayoría |
| Usuario | `administrator` | Requerido para instalación |
| Red | Alcanzable desde Motherbee | IP o hostname |

### 5.3 Credenciales

```rust
// Hardcoded en el sistema - NO configurable
pub const BOOTSTRAP_SSH_USER: &str = "administrator";
pub const BOOTSTRAP_SSH_PASS: &str = "magicAI";
pub const BOOTSTRAP_SSH_PORT: u16 = 22;
```

El password `magicAI` es el **"cordón umbilical"** - solo funciona para el bootstrap inicial. Después queda deshabilitado.

### 4.4 API

**HTTP (via SY.admin):**
```
POST /hives
Content-Type: application/json

{
  "hive_id": "staging",
  "address": "192.168.1.50"
}
```

**Mensaje interno (SY.admin → SY.orchestrator):**
```json
{
  "routing": {
    "src": "<uuid-sy-admin>",
    "dst": "SY.orchestrator@motherbee",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "admin",
    "action": "add_hive",
    "target": "SY.orchestrator@motherbee"
  },
  "payload": {
    "hive_id": "staging",
    "address": "192.168.1.50"
  }
}
```

Nota: cuando `SY.admin` conoce el UUID local de `SY.orchestrator`, puede enviar `routing.dst` por UUID para optimizar; `meta.target` mantiene el destino canónico por nombre L2.

### 4.5 Flujo Detallado de add_hive

```
┌──────────────────────────────────────────────────────────────────┐
│ PASO 1: Validación                                               │
├──────────────────────────────────────────────────────────────────┤
│ • Verificar que hive_id no exista ya                          │
│ • Verificar formato de address (IP o hostname)                  │
│ • Si error → responder HIVE_EXISTS o INVALID_ADDRESS          │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 2: Conexión SSH                                             │
├──────────────────────────────────────────────────────────────────┤
│ • Conectar a administrator@{address}:22                                  │
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
│ • Guardar en /var/lib/fluxbee/hives/{hive_id}/          │
│   ├── ssh.key      (privada, permisos 600)                      │
│   └── ssh.key.pub  (pública)                                    │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 4: Configurar SSH en máquina remota                         │
├──────────────────────────────────────────────────────────────────┤
│ Via SSH con password:                                            │
│ • mkdir -p /home/administrator/.ssh                                           │
│ • Agregar key pública a /home/administrator/.ssh/authorized_keys              │
│ • chmod 600 /home/administrator/.ssh/authorized_keys                          │
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
│ • mkdir -p /etc/fluxbee                                      │
│ • mkdir -p /var/lib/fluxbee/nodes                           │
│ • mkdir -p /var/lib/fluxbee/opa-rules                       │
│ • mkdir -p /var/run/fluxbee/routers                         │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 7: Crear hive.yaml                                        │
├──────────────────────────────────────────────────────────────────┤
│ Crear /etc/fluxbee/hive.yaml:                             │
│                                                                  │
│   hive_id: staging                                            │
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
│ Crear /etc/fluxbee/config-routes.yaml:                      │
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
│ En mother hive:                                                │
│ • Esperar que RT.gateway reciba conexión de staging             │
│ • Esperar HELLO + LSA de la isla remota                         │
│ • Verificar en jsr-lsa-<hive> que staging aparece             │
│ • Timeout: 60s                                                  │
│ • Si timeout → responder WAN_TIMEOUT (pero isla queda instalada)│
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 12: Registrar en repo de islas                              │
├──────────────────────────────────────────────────────────────────┤
│ Crear /var/lib/fluxbee/hives/{hive_id}/info.yaml:       │
│                                                                  │
│   hive_id: staging                                            │
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
    "hive_id": "staging",
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
    "message": "No se pudo conectar. Verificar: user=administrator, pass=magicAI, port=22"
  }
}
```

### 4.7 Códigos de Error

| Código | Paso | Descripción |
|--------|------|-------------|
| `HIVE_EXISTS` | 1 | Ya existe isla con ese ID |
| `INVALID_ADDRESS` | 1 | Formato de dirección inválido |
| `SSH_TIMEOUT` | 2 | Host no responde en 10s |
| `SSH_CONNECTION_REFUSED` | 2 | Puerto 22 cerrado |
| `SSH_AUTH_FAILED` | 2 | Password incorrecto (no es "magicAI") |
| `SSH_KEY_FAILED` | 4 | Error configurando SSH key |
| `COPY_FAILED` | 5 | Error copiando binarios |
| `SERVICE_FAILED` | 10 | Error iniciando systemd service |
| `WAN_TIMEOUT` | 11 | Isla no conectó por WAN en 60s |

### 4.8 Resultado Final

Después de `add_hive` exitoso:

**En mother hive:**
```
/var/lib/fluxbee/hives/staging/
├── ssh.key           # Para acceso SSH de emergencia
├── ssh.key.pub
└── info.yaml         # Metadata de la isla
```

**En isla remota (staging):**
```
/etc/fluxbee/
└── hive.yaml       # Con uplink a mother (sin admin)

/var/lib/fluxbee/
├── identity.yaml
├── orchestrator.yaml   # Estado interno (no fuente de config)
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
# Desde mother hive
ssh -i /var/lib/fluxbee/hives/staging/ssh.key administrator@192.168.1.50
```

---

## 5. API del Orchestrator

### 5.1 Gestión de Islas Remotas (Fase 1)

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `POST /hives` | `add_hive` | Bootstrap isla remota |
| `GET /hives` | `list_hives` | Lista islas hijas |
| `GET /hives/{id}` | `get_hive` | Info de isla específica |
| `DELETE /hives/{id}` | `remove_hive` | Elimina registro (no apaga isla) |

### 5.2 Gestión de Nodos/Router por Hive (Canónico)

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /hives/{id}/nodes` | `list_nodes` | Lista nodos de hive |
| `POST /hives/{id}/nodes` | `run_node` | Levanta nodo en hive |
| `DELETE /hives/{id}/nodes/{name}` | `kill_node` | Mata nodo en hive |
| `GET /hives/{id}/routers` | `list_routers` | Lista routers de hive |
| `POST /hives/{id}/routers` | `run_router` | Levanta router en hive |
| `DELETE /hives/{id}/routers/{name}` | `kill_router` | Mata router en hive |

### 5.3 Estado de Isla

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /health` | - | Health check básico |
| `GET /hive/status` | `hive_status` | Estado completo de la isla |

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
│  /var/lib/fluxbee/               /mnt/jsr-shared/           │
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

**Default:** `/var/lib/fluxbee` (local, hijas piden por HTTP)

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
¿Existe en /var/lib/fluxbee/modules/AI.soporte/1.2.0/?
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
2. Persiste en `/etc/fluxbee/hive.yaml` (`storage.path`)
3. Envía CONFIG_RESPONSE a SY.admin
4. Próximas operaciones de módulos usan el nuevo path

**CONFIG_RESPONSE:**

```json
{
  "routing": {
    "src": "<uuid-sy-orchestrator>",
    "dst": "<uuid-sy-admin>",
    "ttl": 16,
    "trace_id": "<uuid-del-config-changed>"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_RESPONSE"
  },
  "payload": {
    "subsystem": "storage",
    "version": 1,
    "hive": "staging",
    "node": "SY.orchestrator@staging",
    "status": "ok"
  }
}
```

### 6.6 Transición HTTP → NFS

1. **Infra monta NFS** en todas las máquinas (mother + hijas) en el mismo path
2. **Copiar datos** de mother al NFS (una vez):
   ```bash
   cp -r /var/lib/fluxbee/modules /mnt/jsr-shared/
   cp -r /var/lib/fluxbee/blob /mnt/jsr-shared/
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
Description=JSON Router Hive Orchestrator
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
echo 'hive_id: produccion' > /etc/fluxbee/hive.yaml
systemctl enable --now sy-orchestrator

# Agregar isla remota (desde mother)
curl -X POST http://localhost:8080/hives \
  -H "Content-Type: application/json" \
  -d '{"hive_id": "staging", "address": "192.168.1.50"}'
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
| SY.identity | ❌ No | Crítico para L3/ILKs |
| AI.*, WF.*, IO.* | ✅ Sí | Nodos de aplicación |

---

## 9. Troubleshooting

### 9.1 Isla no arranca

```bash
# Verificar config
cat /etc/fluxbee/hive.yaml

# Ejecutar manualmente con debug
JSR_LOG_LEVEL=debug /usr/bin/sy-orchestrator

# Ver logs
journalctl -u sy-orchestrator -f
```

### 9.2 add_hive falla

```bash
# Verificar conectividad
ping 192.168.1.50

# Verificar SSH manual
ssh administrator@192.168.1.50
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
cat /etc/fluxbee/hive.yaml

# Verificar logs del gateway
journalctl -u sy-orchestrator | grep -i wan
```

### 9.4 Acceder a isla remota post-bootstrap

```bash
# Desde mother hive
ssh -i /var/lib/fluxbee/hives/staging/ssh.key administrator@192.168.1.50
```

### 9.5 Reset completo de isla

```bash
systemctl stop sy-orchestrator
rm -rf /var/lib/fluxbee/*
rm -rf /var/run/fluxbee/*
rm -f /dev/shm/jsr-*
# Mantener /etc/fluxbee/hive.yaml
systemctl start sy-orchestrator
```

---

## 10. Defaults del Sistema

Valores hardcodeados, no configurables:

```rust
// Paths
pub const CONFIG_DIR: &str = "/etc/fluxbee";
pub const STATE_DIR: &str = "/var/lib/fluxbee";
pub const RUN_DIR: &str = "/var/run/fluxbee";
pub const SHM_PREFIX: &str = "/jsr-";

// Archivos
pub const HIVE_CONFIG: &str = "/etc/fluxbee/hive.yaml";
pub const ROUTES_CONFIG: &str = "/etc/fluxbee/config-routes.yaml";
pub const IDENTITY_FILE: &str = "/var/lib/fluxbee/identity.yaml";

// Bootstrap SSH
pub const BOOTSTRAP_SSH_USER: &str = "administrator";
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
    "SY.identity",
    "SY.admin",
];

// Default gateway name
pub const DEFAULT_GATEWAY_NAME: &str = "RT.gateway";
```

---

## 11. Fases de Implementación

| Fase | Funcionalidad | Estado |
|------|---------------|--------|
| **1** | `add_hive` - Bootstrap SSH de islas remotas | Prioridad |
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
