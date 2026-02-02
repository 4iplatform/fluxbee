# SY.orchestrator - Tareas pendientes vs spec

Checklist para implementar SY.orchestrator según `docs/07-operaciones.md` y `docs/01-arquitectura.md`.

## 1) Bootstrap local (Fases 0–4)
- [x] Leer `/etc/json-router/island.yaml` y validar `island_id`.
- [x] Crear estructura de directorios (paths fijos de `07-operaciones`).
- [x] Escribir PID en `/var/run/json-router/orchestrator.pid`.
- [ ] Levantar `RT.gateway` (systemd o exec) y esperar socket/shm (timeout 30s).
- [ ] Levantar en paralelo: `SY.config.routes`, `SY.opa.rules`, `SY.admin`, `SY.identity` y esperar conexión (timeout 30s).
- [ ] Conectar como nodo `SY.orchestrator@<isla>` (HELLO/ANNOUNCE) y entrar al loop principal.
- [ ] Log `Island {island_id} ready`.

## 2) Watchdog y lifecycle
- [ ] Tick cada 5s: verificar que RT.gateway y SY.* sigan vivos.
- [ ] Reiniciar automáticamente RT.gateway, SY.config.routes, SY.opa.rules, SY.admin, SY.identity.
- [ ] Para AI/WF/IO: solo log warning (no auto-restart).
- [ ] Shutdown ordenado (SIGTERM): AI/WF/IO → SY.identity + SY.* → RT.gateway, con espera 10s.

## 3) API interna (mensajes admin)
- [x] `list_nodes` (SHM router) / `run_node`, `kill_node` (stubs).
- [x] `list_routers` (SHM router) / `run_router`, `kill_router` (stubs).
- [x] `island_status` (estado completo de la isla).
- [x] `get_storage` (path actual).
- [x] `set_storage` via CONFIG_CHANGED `subsystem=storage` (aplicar + persistir).
- [x] `set_storage` responde `CONFIG_RESPONSE` (según spec actualizada).

## 4) Storage (orchestrator.yaml)
- [x] Mantener `storage.path` en memoria.
- [x] Persistir en `/etc/json-router/orchestrator.yaml`.
- [x] Usar path default `/var/lib/json-router` si no hay config.

## 5) add_island (bootstrap remoto)
- [x] Validar `island_id` y `address` (errores: `ISLAND_EXISTS`, `INVALID_ADDRESS`).
- [ ] SSH root@{address}:22 con pass `magicAI` (timeout 10s).
- [ ] Generar key ed25519 en `/var/lib/json-router/islands/{id}/` (600).
- [ ] Configurar SSH remoto: authorized_keys + disable password + restart sshd.
- [ ] Copiar binarios `/usr/bin/sy-*` y `rt-*` (scp) + chmod +x.
- [ ] Crear dirs remotos `/etc/json-router`, `/var/lib/json-router/*`, `/var/run/json-router/routers`.
- [ ] Crear `/etc/json-router/island.yaml` remoto (uplink hacia mother).
- [ ] Crear `/etc/json-router/config-routes.yaml` vacío.
- [ ] Instalar `sy-orchestrator.service` y `systemctl enable`.
- [ ] `systemctl start sy-orchestrator`.
- [ ] Esperar WAN (60s), validar LSA; si falla → `WAN_TIMEOUT`.
- [ ] Registrar `/var/lib/json-router/islands/{id}/info.yaml` (status, address, created_at).

## 6) Repo de islas
- [x] `list_islands`, `get_island`, `remove_island` (solo registro local).

## 7) Consideraciones
- [x] SY.orchestrator es proceso raíz y único por isla.
- [x] Los procesos levantados no son hijos (usarlo en el diseño de watchdog).
- [x] Mantener compatibilidad con `SY.admin` (acciones actuales ya esperan estas respuestas).
- [x] SY.identity agregado a bootstrap/watchdog/shutdown y no-killable.
- [x] jsr-identity-<island> está documentado y corresponde al writer SY.identity.
- [x] subsystem=storage → SY.orchestrator documentado.
