# Lab Proxmox — cómo usarlo (operación diaria)

Guía operativa del lab de VMs. Complementa (no duplica):

- [`docs/packaging-and-build.md`](../docs/packaging-and-build.md) — §3 **agregar un nodo al install**
  (una línea en `base-nodes.json`), §4 build box / `.deb` / repo apt, §4.4 layout de los 3 servers.
- [`docs/14-runtime-rollout-motherbee.md`](../docs/14-runtime-rollout-motherbee.md) — **EL runbook**
  para publicar/actualizar/spawnear un runtime en un backend YA instalado (publish → manifest →
  `/update` → `/nodes`). No inventar rutas alternativas: es este.
- [`lab/README.md`](README.md) — el lab **Docker** (otra cosa; sin VMs).

## Mapa (quién es quién)

| VM | Host Proxmox | Nombre | Rol |
|----|--------------|--------|-----|
| 210 | PC-004-165 | fb-build | **Máquina de COMPILAR** + repo apt `:8900`. Repo en `/opt/fluxbee`. |
| 240 | PC-004-157 | fbi-mb | Motherbee del hive integrado (admin API `:8080` local). |
| 241 | PC-004-157 | fbi-worker | Worker (caja limpia, bootstrapeada por MB — sin .deb, sin DB). |
| 242 | PC-004-157 | fbi-ingress | Ingress (DMZ). |
| 243 | PC-004-157 | fbi-egress | Egress. |
| 250/9000 | — | templates | No tocar (clonar, no arrancar). |

Los 3 hosts (`PC-004-165/156/157`) forman un cluster: cualquier `PVE_HOST` responde por todos,
pero **`PVE_NODE` es obligatorio** para operar VMs (indica en qué host vive la VM).

## Acceso: `lab/pve.py` (guest-agent, sin SSH)

Todo se opera por la API de Proxmox + qemu-guest-agent (exec/push/pull **como root** dentro de la
VM). No hay SSH a las VMs para operación de lab.

```bash
export PVE_HOST=192.168.4.165          # cualquier host del cluster
export PVE_TOKEN='<user>@pve!<tokenid>=<secret>'   # token API (pedirlo al operador; NUNCA commitearlo)

# VMs de un host
PVE_NODE=PC-004-157 python3 lab/pve.py list

# Ejecutar dentro de una VM (root)
PVE_NODE=PC-004-157 python3 lab/pve.py exec 240 -- 'systemctl is-active sy-orchestrator'

# Copiar archivos local <-> VM
PVE_NODE=PC-004-165 python3 lab/pve.py pull 210 /ruta/en/vm ./local
PVE_NODE=PC-004-157 python3 lab/pve.py push 240 ./local /ruta/en/vm

# Snapshot / rollback (antes de algo destructivo)
PVE_NODE=PC-004-157 python3 lab/pve.py snapshot 240 antes-de-X
PVE_NODE=PC-004-157 python3 lab/pve.py rollback 240 antes-de-X
```

Más comandos: docstring de [`lab/pve.py`](pve.py). Gotcha conocida: el qga de la VM240 puede
ponerse flaky bajo carga — se recupera con `status`/`reset` duro.

## Compilar: SIEMPRE en fb-build (VM210)

Acá **no se compila en la Mac ni en el MB**: el build box documentado es fb-build
(toolchain completo, `source /etc/profile.d/fluxbee-toolchain.sh`, repo en `/opt/fluxbee`).

```bash
# Actualizar el repo del build box a la rama pusheada
PVE_NODE=PC-004-165 python3 lab/pve.py exec 210 -- \
  'cd /opt/fluxbee && git fetch origin <rama> && git checkout <rama> && git reset --hard origin/<rama>'

# Un binario suelto (ej. un runtime IO; el workspace io es aparte)
PVE_NODE=PC-004-165 python3 lab/pve.py exec 210 -- \
  'source /etc/profile.d/fluxbee-toolchain.sh; cd /opt/fluxbee && cargo build --release --manifest-path nodes/io/Cargo.toml -p <crate>'

# El .deb completo (lee base-nodes.json; ver docs/packaging-and-build.md §4.2-4.3)
PVE_NODE=PC-004-165 python3 lab/pve.py exec 210 -- 'bash /root/build2.sh'   # o scripts/make-deb.sh
```

## Llevar un runtime nuevo al hive 240 (SIN .deb nuevo)

Camino canónico = [`docs/14-runtime-rollout-motherbee.md`](../docs/14-runtime-rollout-motherbee.md).
Resumen para el caso target = el propio motherbee (VM240):

```bash
# 1. binario: fb-build -> VM240 (pull a local, push a la VM)
PVE_NODE=PC-004-165 python3 lab/pve.py pull 210 /opt/fluxbee/nodes/io/target/release/<bin> /tmp/<bin>
PVE_NODE=PC-004-157 python3 lab/pve.py push 240 /tmp/<bin> /tmp/<bin>

# 2. publish + manifest en el MB (runbook §2-3) — publish-runtime.sh es genérico (--runtime/--binary)
# 3. POST /hives/motherbee/update category=runtime (runbook §6)
# 4. POST /hives/motherbee/nodes  {"node_name":"...","runtime":"...","runtime_version":"current","tenant_id":...}  (runbook §7)
# 5. verificar: unit activa + CONFIG_GET (UNCONFIGURED hasta cargar el secreto en SY.vault)
```

Un nodo del **set base** (`packaging/base-nodes.json`) entra al `.deb` automáticamente
(`build-deb.sh` lee el manifest — **no se toca el script de build**) y `fluxbee-firstboot` lo
spawnea en un install desde cero. El canal de runtimes de arriba es para sumarlo a un backend
que ya está corriendo.

## Reglas

- **Buscar la doc antes de operar** (este archivo + los dos runbooks de arriba). El camino de
  integrar/publicar nodos YA EXISTE; no crear scripts/rutas accesorias por nodo.
- Secretos (tokens PVE, keys) van por env/vault — jamás en el repo ni en logs.
- Solo el MB (240) tiene .deb/DB; workers/ingress/egress son cajas limpias bootstrapeadas por el
  MB (`add_hive`) — instalar postgres o el .deb en un worker es un bug, no un fix.
