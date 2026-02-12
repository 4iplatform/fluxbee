# SY.orchestrator - Tareas pendientes vs spec

Checklist para implementar SY.orchestrator según `docs/07-operaciones.md` y `docs/01-arquitectura.md`.

## 1) Bootstrap local (Fases 0–4)
- [x] Leer `/etc/fluxbee/island.yaml` (fallback legacy) y validar `island_id`.
- [x] Crear estructura de directorios (paths fijos de `07-operaciones`).
- [x] Escribir PID en `/var/run/fluxbee/orchestrator.pid` (fallback legacy).
- [x] Levantar `RT.gateway` (systemd o exec) y esperar socket/shm (timeout 30s).
- [x] Levantar en paralelo: `SY.config.routes`, `SY.opa.rules`, `SY.admin`, `SY.storage` y `SY.identity` opcional si existe binario.
- [x] Conectar como nodo `SY.orchestrator@<isla>` (HELLO/ANNOUNCE) y entrar al loop principal.
- [x] Log `Island {island_id} ready`.

## 2) Watchdog y lifecycle
- [x] Tick cada 5s: verificar que RT.gateway y SY.* sigan vivos.
- [x] Reiniciar automáticamente RT.gateway, SY.config.routes, SY.opa.rules, SY.admin, SY.storage y SY.identity si aplica.
- [x] Para AI/WF/IO: solo log warning (no auto-restart).
- [x] Shutdown ordenado (SIGTERM): AI/WF/IO → SY.* (+SY.identity si aplica) → RT.gateway, con espera 10s.

## 3) API interna (mensajes admin)
- [x] `list_nodes` (SHM router) / `run_node`, `kill_node` (stubs).
- [x] `list_routers` (SHM router) / `run_router`, `kill_router` (stubs).
- [x] `island_status` (estado completo de la isla).
- [x] `get_storage` (path actual).
- [x] `set_storage` via CONFIG_CHANGED `subsystem=storage` (aplicar + persistir).
- [x] `set_storage` responde `CONFIG_RESPONSE` (según spec actualizada).

## 4) Storage (orchestrator.yaml)
- [x] Mantener `storage.path` en memoria.
- [x] Persistir en `/var/lib/fluxbee/orchestrator.yaml` (fallback legacy).
- [x] Usar path default `/var/lib/fluxbee` si no hay config.

## 5) add_island (bootstrap remoto)
- [x] Validar `island_id` y `address` (errores: `ISLAND_EXISTS`, `INVALID_ADDRESS`).
- [x] SSH `administrator@{address}:22` con pass `magicAI` (timeout 10s).
- [x] Generar key ed25519 en `/var/lib/fluxbee/islands/{id}/` (600).
- [ ] Configurar SSH remoto: authorized_keys + disable password + restart sshd. (password disable pendiente)
- [x] Copiar binarios `/usr/bin/sy-*` y `rt-*` (scp) + chmod +x.
- [x] Crear dirs remotos `/etc/fluxbee`, `/var/lib/fluxbee/*`, `/var/run/fluxbee/routers`.
- [x] Crear `/etc/fluxbee/island.yaml` remoto (uplink hacia mother).
- [x] Crear `/etc/fluxbee/sy-config-routes.yaml` vacío.
- [x] Instalar units worker (`rt-gateway`, `sy-config-routes`, `sy-opa-rules`, `sy-identity` opcional) y `systemctl enable`.
- [x] Iniciar units worker remotas (sin `sy-orchestrator` en worker).
- [x] Esperar WAN (60s), validar LSA; si falla → `WAN_TIMEOUT`.
- [x] Registrar `/var/lib/fluxbee/islands/{id}/info.yaml` (status, address, created_at).

## 6) Repo de islas
- [x] `list_islands`, `get_island`, `remove_island` (solo registro local).

## 7) Consideraciones
- [x] SY.orchestrator es proceso raíz y único por isla.
- [x] Los procesos levantados no son hijos (usarlo en el diseño de watchdog).
- [x] Mantener compatibilidad con `SY.admin` (acciones actuales ya esperan estas respuestas).
- [x] SY.identity se gestiona si el binario está instalado (ctx pendiente en v1.16).
- [x] jsr-identity-<island> está documentado y corresponde al writer SY.identity.
- [x] subsystem=storage → SY.orchestrator documentado.
- [x] Estandarizar runtime names en `/usr/bin` (`sy-admin`, `sy-config-routes`, `sy-orchestrator`, `sy-storage`) con fallback de build (`sy_admin`, etc.).
