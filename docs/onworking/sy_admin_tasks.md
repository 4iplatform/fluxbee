# SY.admin - Estado actual vs spec (v1.16+)

> Nota de direccion (2026-03-02): las tareas activas para la arquitectura nueva (orchestrator v2, `SYSTEM_UPDATE`, control-plane socket L2) estan integradas en `docs/onworking/sy_orchestrator_v2_tasks.md`. Este archivo se mantiene como referencia historica de v1.16.
>
> Actualizacion de contrato (2026-03-13): `SY.admin` ya no expone `/hives/{id}/routers*`. La operacion canónica es por `/hives/{id}/nodes` (`SPAWN_NODE`/`KILL_NODE`). Cualquier mención de `/routers*` en este documento debe leerse como histórico.

Checklist operativo consolidado para `SY.admin`.

## Cerrado (histórico consolidado)
Lista de tareas cerradas para alinear `SY.admin` con la especificacion actual y con el modelo motherbee/worker.

## Cerrado
- [x] Endpoints REST de hives implementados:
  - [x] `POST /hives`
  - [x] `GET /hives`
  - [x] `GET /hives/{id}`
  - [x] `DELETE /hives/{id}`
- [x] `GET /hive/status`.
- [x] `GET/PUT /config/storage` con `CONFIG_CHANGED` (`subsystem=storage`).
- [x] API de modulos: `/modules`, `/modules/{name}`, `/modules/{name}/{version}`.
- [x] Correlacion request/response por `trace_id` para admin y OPA.
- [x] OPA target broadcast/unicast alineado y timeout de OPA en 30s.

## Pendiente critico (impacta pruebas)
- [x] Corregir routing multi-hive de acciones de nodos:
  - `/hives/{hive}/nodes` ahora enruta a orchestrator local (`SY.orchestrator@motherbee`).
  - Se propaga `target` en payload para que orchestrator ejecute sobre hive remota.
- [x] Corregir listado multi-hive de nodos:
  - `GET /hives/{hive}/nodes` devuelve vista del hive target (no snapshot local de motherbee).
- [x] Corregir contrato de payload para `kill_node`:
  - HTTP mantiene `{"name": ...}` por compatibilidad.
  - `SY.admin` normaliza a `node_name` antes de enviar a orchestrator.
- [x] Eliminar contrato mutante de `kill_router` al retirar endpoints `/routers*`.

## Pendiente alto (consistencia API)
- [x] Definir y aplicar version monotona para `CONFIG_CHANGED` en routes/vpns/storage.
  - `routes` y `vpns` comparten stream monotono (`routes-vpns`) para evitar conflictos en `SY.config.routes`.
  - `storage` usa stream monotono separado.
  - Persistencia local en `state/config_versions/*.txt` (sobre `json_router::paths::state_dir()`).
  - Si se envia `version` manual <= actual, responde `409 VERSION_MISMATCH`.
- [x] Unificar formato de respuesta HTTP y codigos:
  - `SY.admin` ahora mapea `error_code -> HTTP status` (400/404/409/422/501/502/503/504).
  - respuestas de admin/OPA incluyen envelope consistente con `status`, `action`, `payload`, `error_code`, `error_detail`.
- [x] Eliminar coexistencia de rutas legacy para nodos/routers y dejar estrategia canónica:
  - removidos handlers legacy `/nodes` y `/routers` en `SY.admin`.
  - canónico único: `/hives/{id}/nodes`.

## Pendiente medio
- [x] Revalidar contrato `add_hive` desde API con matriz de errores esperados de spec.
  - mapeo HTTP en `SY.admin` ajustado para `SSH_*`, `INVALID_HIVE_ID`, `MISSING_WAN_LISTEN`, `COPY_FAILED`, `CONFIG_FAILED`.
  - smoke E2E agregado: `scripts/admin_add_hive_matrix.sh`.
  - checklist actualizado con cobertura manual de `WAN_TIMEOUT`.
- [x] Agregar pruebas de integracion end-to-end para:
  - [x] `/hives/{id}/nodes` (run/kill)
  - [x] `/config/storage` (broadcast + confirmacion)
  - Nota: `scripts/admin_nodes_routers_storage_e2e.sh` quedó histórico porque valida `/routers*` (contrato removido en v2).

## Pendiente nuevo - lifecycle de runtimes por REST

Motivación:
- Hoy `SY.admin` permite instalar software indirectamente vía `fluxbee-publish`, y permite operar instancias vía `/hives/{id}/nodes`.
- Falta el complemento REST para remover artifacts de runtime publicados en `dist`/manifest sin tocar el CLI ni editar archivos manualmente.
- La operación canónica debería vivir en `SY.admin` y pasar por el mismo modelo HTTP + gateway interno que el resto de las acciones administrativas.

Alcance propuesto:
- administrar artifacts publicados (manifest + `dist`)
- no reemplaza `kill_node`
- no reemplaza `GET /versions` (esa sigue siendo la vista efectiva/readiness)

Contrato operativo cerrado:
- ownership: sólo `motherbee` modifica `manifest.json` y `dist/runtimes`
- replicación: los workers convergen por Syncthing; un worker desconectado no bloquea delete
- serialización: publish/remove/update de runtimes deben ejecutarse bajo lock secuencial; si hay otra operación activa, responder `BUSY`
- criterio de uso:
  - un runtime/version está `in use` si existe algún nodo `RUNNING` en el inventario global usando ese runtime/version
  - no se usa la mera existencia de `config.json` como bloqueo para delete de artifacts
  - la evaluación sale del inventario visible del control-plane
  - un hive `offline/stale` no bloquea delete; si está desconectado, no cuenta como `RUNNING`
- criterio de dependencia:
  - un runtime base no puede borrarse si existen runtimes publicados `config_only` o `workflow` cuyo `runtime_base` lo referencia
  - este check sale del manifest/`GET /versions`, no del inventario de nodos
- política de current:
  - no se permite borrar la versión `current`
  - no hay fallback automático ni promote implícito
- atomicidad:
  - el delete es atómico en `motherbee`
  - luego la remoción converge al resto de hives por replicación
  - no se intenta atomicidad distribuida estricta entre hives

Explicación corta del modelo:
- `kill_node` y `DELETE /hives/{hive}/nodes/{name}` siguen siendo lifecycle de proceso/instancia, no de artifact publicado
- el delete de runtimes opera sobre el catálogo publicado en `motherbee`
- para decidir si puede borrar:
  - se mira el inventario global para `in use`
  - se mira el manifest/`/versions` para dependencias `runtime_base`
- si pasa ambos checks, se borra en `motherbee` y el resto converge solo
- si un worker estaba desconectado al momento del delete, la reconciliación cuando vuelva queda explícitamente fuera de alcance de esta v1; el owner ya removió el artifact y la convergencia posterior se observará aparte

Endpoints candidatos vNext:
- [x] `GET /hives/{hive}/runtimes`
  - lista runtimes publicados visibles para ese hive, incluyendo `current`, `available`, `type`, `runtime_base` y readiness resumido
- [x] `GET /hives/{hive}/runtimes/{runtime}`
  - detalle por runtime, incluyendo versiones publicadas y readiness por versión
- [x] `DELETE /hives/{hive}/runtimes/{runtime}/versions/{version}`
  - remueve una versión puntual del manifest/dist
  - sólo permitido para versiones no-`current`
  - la remoción converge luego al resto del grupo

Fuera de alcance en v1:
- [ ] `DELETE /hives/{hive}/runtimes/{runtime}`
  - removido del alcance inicial
  - choca con la regla `no current delete`
  - si se necesita más adelante, requerirá contrato explícito de deprecación/desasignación de `current`

Decisiones de contrato resueltas:
- [x] ownership real:
  - sólo `motherbee` modifica manifest/dist
  - `hive` en el endpoint se interpreta como contexto operativo/consulta, no como owner alternativo de artifacts
- [x] política de seguridad:
  - rechazar delete si existe algún nodo `RUNNING` usando ese runtime/version
  - rechazar delete de runtime base si hay `config_only`/`workflow` dependientes publicados
  - rechazar delete de `current`
- [x] semántica de reconcile:
  - delete muta primero `motherbee`
  - workers offline no bloquean
  - el resultado debe reportar convergencia posterior, no atomicidad distribuida
- [x] definición conceptual de vista:
  - `GET /hives/{hive}/runtimes` puede ser recurso nuevo, pero su fuente de verdad es el manifest/readiness que hoy ya alimenta `/versions`
- [x] definir shape final de respuesta:
  - runtime/version removido
  - manifest_version nuevo
  - manifest_hash nuevo
  - `replication_status` / `convergence_status`
  - hives pendientes si aplica

Orden sugerido de implementación:
1. resolver chequeo `RUNTIME_IN_USE`
   - leer inventario global visible del control-plane
2. resolver chequeo `RUNTIME_HAS_DEPENDENTS`
   - leer manifest/runtime catalog
3. `DELETE /hives/{hive}/runtimes/{runtime}/versions/{version}`
   - sólo no-`current`
   - lock secuencial
4. E2E y negativos

Checklist de implementación propuesto:
- [x] agregar acciones internas en `SY.admin` registry:
  - [x] `list_runtimes`
  - [x] `get_runtime`
  - [x] `remove_runtime_version`
- [x] implementar handlers REST en `SY.admin` para lectura:
  - [x] `GET /hives/{hive}/runtimes`
  - [x] `GET /hives/{hive}/runtimes/{runtime}`
- [x] implementar handler REST de delete en `SY.admin`
- [x] delegar ejecución a `SY.orchestrator` (owner de manifest/dist lifecycle)
- [x] endurecer mapeo HTTP de errores:
  - `RUNTIME_NOT_FOUND`
  - `RUNTIME_VERSION_NOT_FOUND`
  - `RUNTIME_IN_USE`
  - `RUNTIME_HAS_DEPENDENTS`
  - `RUNTIME_CURRENT_CONFLICT`
  - `BUSY`
  - `RUNTIME_REMOVE_FAILED`
- [x] implementar lock secuencial de lifecycle manifest/dist en motherbee
- [x] definir cómo se resuelve `RUNTIME_IN_USE`
  - fuente: inventario global
  - criterio: nodos `RUNNING`
  - incluir version exacta, no sólo nombre de runtime
- [x] definir cómo se resuelve `RUNTIME_HAS_DEPENDENTS`
  - fuente: manifest/runtime catalog
  - criterio: runtimes publicados con `runtime_base=<runtime>`
- [x] agregar E2E de paridad HTTP/socket para remove runtime version
- [x] agregar E2E negativos básicos:
  - borrar runtime en uso
  - borrar base runtime con dependientes
  - borrar versión inexistente
  - borrar `current`
- [x] cubrir `BUSY` de lifecycle lock
  - test interno determinista en `SY.orchestrator`
  - nota: no es observable hoy por HTTP porque `SY.admin` serializa commands y `SY.orchestrator` procesa admin actions secuencialmente

Estado actual de implementación:
- `GET /hives/{hive}/runtimes/{runtime}` ya expone:
  - `usage` local visible
  - `usage_global_visible` agregado por control-plane
- `DELETE /hives/{hive}/runtimes/{runtime}/versions/{version}` ya:
  - corre sólo en `motherbee`
  - rechaza `current`
  - rechaza si hay `RUNNING` visible para esa versión
  - rechaza si el runtime tiene dependientes `config_only`/`workflow`
  - muta manifest + `dist` con lock de lifecycle y rollback local básico si falla la escritura del manifest
- Script E2E/negativos agregado:
  - `scripts/admin_runtime_delete_e2e.sh`
  - cubre `ok`, `RUNTIME_CURRENT_CONFLICT`, `RUNTIME_VERSION_NOT_FOUND`, `RUNTIME_IN_USE`, `RUNTIME_HAS_DEPENDENTS`
  - incluye paridad HTTP/socket para `remove_runtime_version`
- Cobertura interna adicional:
  - `remove_runtime_version_flow_returns_busy_when_lifecycle_lock_is_held`
  - justificación: `BUSY` no emerge por HTTP en la arquitectura actual porque el path `SY.admin -> SY.orchestrator` ya serializa las operaciones antes del lifecycle lock

## Matriz operativa - CORE/CUSTOM x singleton/instanciado

Cruce pedido para ordenar el modelo real del sistema.

Ejes:
- Tipo de ejecutable:
  - `CORE`: ejecutables del sistema/OS (`RT.*`, `SY.*`, bootstrap del hive, servicios base)
  - `CUSTOM`: runtimes externos publicados en `dist` e integrados para correr bajo control de Fluxbee
- Modo de ejecución:
  - `singleton`: una única instancia por rol/nombre esperado
  - `instanciado`: mismo ejecutable/runtime corriendo varias veces con distintos `node_name`

### Estado actual por caso

| Caso | Estado actual | Qué ya está resuelto | Qué sigue abierto |
|------|---------------|----------------------|-------------------|
| `CORE + singleton` | **Mayormente resuelto** | bootstrap escribe units persistentes en `/etc/systemd/system`, hace `daemon-reload`, habilita y arranca bootstrap units; `SYSTEM_UPDATE` cubre rollout core | terminar de dejar explícita la semántica completa de lifecycle REST para core cuando corresponda |
| `CORE + instanciado` | **No es modelo explícito hoy** | `run_node` acepta nombres arbitrarios, pero el modelo canónico de `config.json/state.json` declara `SY.*` y `RT.*` fuera de scope | falta decisión de producto: prohibirlo formalmente o soportarlo como clase explícita |
| `CUSTOM + singleton` | **Parcialmente resuelto** | package `full_runtime/config_only/workflow`, publish/install, `--deploy`, readiness, spawn, config persistida, identity wiring, ejecución real validada | falta relaunch/autostart post-reboot; falta inventario de instancias gestionadas desacoplado del router; falta remove real de instancia |
| `CUSTOM + instanciado` | **Parcialmente resuelto** | runtime único con múltiples `node_name`, `config.json` por instancia, `_system` inyectado, `kill_node/get_node_*` por nombre, E2E real validado con nodos de prueba | mismos huecos que singleton custom: reboot reconcile, inventario persistente de instancias, remove real de instancia |

### Hallazgos concretos de código

- `GET /hives/{hive}/nodes` ya devuelve inventario de instancias gestionadas persistidas leyendo `/var/lib/fluxbee/nodes/**/config.json` y calculando lifecycle local; para hive remoto hace forward al orchestrator remoto.
- `POST /hives/{hive}/nodes` persiste `config.json` en `/var/lib/fluxbee/nodes/<KIND>/<node@hive>/config.json` y hace spawn fail-closed si ya existe.
- `DELETE /hives/{hive}/nodes/{name}` hoy termina en `kill_node`, que hace `systemctl stop/reset-failed`, pero no borra `config.json` ni elimina la instancia persistida.
- El spawn de nodos gestionados usa `systemd-run --collect`, o sea unit transitorio.
- No quedó identificado todavía un reconcile/autostart explícito al arranque que recorra `/var/lib/fluxbee/nodes/**/config.json` y relance workloads custom después de reboot.

### Tareas nuevas derivadas de esta matriz

Revisión 2026-03-19:
- Esta lista sigue vigente.
- No quedó vieja, pero ya no mezcla el frente de lifecycle de runtimes por REST, que quedó resuelto arriba.
- Lo que queda acá es el siguiente frente real: inventario persistente de instancias, remove real de instancia y persistencia/reconcile post-reboot para workloads custom.

- [ ] Definir contrato de producto para `CORE + instanciado`.
  - Opción A: dejarlo explícitamente fuera de scope y rechazar `SY.*` / `RT.*` en el modelo de spawn gestionado.
  - Opción B: soportarlo como clase real con reglas específicas.

- [x] Implementar inventario de instancias gestionadas persistidas.
  - Ya no depende sólo de SHM/router.
  - `GET /hives/{hive}/nodes` pasó a semántica de instancias gestionadas persistidas.
  - Lista lo que existe en `/var/lib/fluxbee/nodes/.../config.json`, aun si el proceso no está conectado.
  - Remoto: forward al orchestrator del hive target.

- [x] Separar lifecycle de instancia: `stop/kill` vs `remove`.
  - `kill_node` sigue siendo stop del proceso/unit.
  - operación canónica nueva para borrar la instancia persistida: `DELETE /hives/{hive}/nodes/{name}/instance`
  - borra `config.json`, `state.json`, `status_version`, `status_fingerprint` y el directorio de instancia
  - libera el `node_name`
  - rechaza si la instancia sigue `RUNNING`/visible (`NODE_INSTANCE_RUNNING`)

- [~] Cerrar el contrato de persistencia post-reboot para workloads `CUSTOM`.
  - modelo elegido: reconcile/autostart desde `SY.orchestrator` al arranque leyendo `/var/lib/fluxbee/nodes/**/config.json`
  - implementado:
    - scan de instancias persistidas
    - salto explícito de `SY.*` / `RT.*`
    - relaunch de instancias `CUSTOM` caídas si tienen `runtime` + `runtime_version` válidos y no están activas/visibles
  - contrato de bootstrap de nombre para runtimes gestionados:
    - `SY.orchestrator` debe pasar el nombre canónico de la instancia al proceso al momento del spawn/relaunch
    - forma elegida para v1: variable de entorno `FLUXBEE_NODE_NAME=<node_name@hive>`
    - no se requiere `FLUXBEE_NODE_CONFIG` como contrato obligatorio en v1
    - el runtime debe poder derivar su `config.json` desde `FLUXBEE_NODE_NAME` usando el layout canónico:
      - `/var/lib/fluxbee/nodes/<KIND>/<node_name@hive>/config.json`
    - `_system.node_name` dentro de `config.json` sigue siendo la fuente de verdad persistida
    - `FLUXBEE_NODE_NAME` es el dato de bootstrap para que el proceso llegue a esa persistencia al arrancar
  - rationale del contrato:
    - evita duplicar dos referencias (`NODE_NAME` + `NODE_CONFIG`) que podrían divergir
    - evita depender de parsing de argumentos en todos los `start.sh`
    - calza bien con `systemd-run --setenv=...`
    - mantiene el cambio concentrado en `SY.orchestrator`, no en cada runtime individual
  - pendiente:
    - validación E2E real post-reboot
    - script preparado: `scripts/custom_node_reboot_reconcile_e2e.sh`
  - ya implementado además:
    - `SY.orchestrator` inyecta `FLUXBEE_NODE_NAME` en spawn y relaunch
    - helper compartido en SDK:
      - `fluxbee_sdk::managed_node_name(...)`
      - `fluxbee_sdk::managed_node_config_path(...)`
    - endurecer reglas finas de elegibilidad si aparece algún caso borde

- [ ] Agregar E2E específico de reboot semantics para workloads custom.
  - publicar runtime
  - spawnear instancia(s)
  - reiniciar host o reiniciar servicios base equivalentes
  - verificar:
    - inventario persistente
  - script propuesto:
    - `scripts/custom_node_reboot_reconcile_e2e.sh`
    - secuencia actual: publish -> spawn -> kill -> wait invisible -> restart `sy-orchestrator` -> verify relaunch -> probe real con `IO.test`
    - relaunch automático si corresponde
    - respuesta correcta de `GET /nodes`, `/config`, `/status`

- [ ] Alinear documentación para separar tres conceptos que hoy se mezclan:
  - runtime instalado/publicado
  - instancia gestionada persistida
  - proceso conectado/vivo en router

## Seguimiento
- [x] Registrar mapeo final de endpoints por ownership:
  - `SY.config.routes`
    - `GET/POST/DELETE /routes`
    - `GET/POST/DELETE /vpns`
    - `GET/POST/DELETE /hives/{hive}/routes`
    - `GET/POST/DELETE /hives/{hive}/vpns`
    - `PUT /config/routes` y `PUT /config/vpns` (ownership funcional de config, aplicado via `CONFIG_CHANGED`).
  - `SY.orchestrator` (via orchestrator local en motherbee para operaciones multi-hive)
    - `GET /hive/status`
    - `GET/PUT /config/storage`
    - `GET/POST /hives`
    - `GET/DELETE /hives/{id}`
    - `GET/POST/DELETE /hives/{hive}/nodes`
  - `SY.opa.rules`
    - `POST /opa/policy`
    - `POST /opa/policy/compile`
    - `POST /opa/policy/apply`
    - `POST /opa/policy/rollback`
    - `POST /opa/policy/check`
    - `GET /opa/policy`
    - `GET /opa/status`
    - `POST /hives/{hive}/opa/policy`
    - `POST /hives/{hive}/opa/policy/compile`
    - `POST /hives/{hive}/opa/policy/apply`
    - `POST /hives/{hive}/opa/policy/rollback`
    - `POST /hives/{hive}/opa/policy/check`
    - `GET /hives/{hive}/opa/policy`
    - `GET /hives/{hive}/opa/status`

## Artefactos de validacion
- [x] Checklist manual E2E con `curl` para API admin/orchestrator: `docs/onworking/sy_admin_tasks.md`.

## Pendientes detectados en revisión spec vs código (2026-02-22)

### P0 - Entrega/configuración confiable
- [x] Hacer que fallo de broadcast/config sea error observable en API (no solo warning en logs).
  - Casos actuales a endurecer:
    - [x] cola de broadcast con `message dropped` (ahora responde `CONFIG_BROADCAST_FAILED`).
    - [x] `broadcast after admin action failed` sin degradar respuesta HTTP (ahora degrada a error explícito).

### P1 - Cobertura real de fanout OPA
- [x] Resolver `expected_hives` con fuente de topología real (LSA/SHM) en lugar de depender solo de `authorized_hives`.
- [x] Diferenciar en respuesta OPA: `pending_hives` esperadas por topología vs autorizadas por política.

### P1 - Homogeneidad de documentación
- [x] Alinear en docs el path canónico del contador de versión OPA con el path real usado por código.
  - Referencia canónica: `json_router::paths::opa_version_path()` => `/var/lib/fluxbee/opa-version.txt`.
- [x] Alinear ejemplo de mensaje interno admin/orchestrator (TTL/dst/meta) con implementación actual.
  - Actualizado ejemplo en `docs/07-operaciones.md` (dst por nombre L2, `ttl=16`, `meta.target` canónico).

### P2 - Hardening operativo (infra)
- [x] Definir perfil seguro por default para `admin.listen` en despliegues productivos (bind local o detrás de proxy).
  - Implementado en `SY.admin`: default `127.0.0.1:8080` cuando no hay `admin.listen` ni `JSR_ADMIN_LISTEN`.
- [x] Documentar política de exposición de API admin (red, proxy, ACL) para evitar despliegues abiertos por error.
  - Sección nueva: `docs/07-operaciones.md` -> "2.6 Exposición del API Admin (perfil seguro)".

## Anexo - Checklist E2E (migrado)
Checklist operativo corto para validar `SY.admin -> SY.orchestrator` via API.

## 0) Base URL

Usar una de estas dos:

```bash
# directo al servicio local
BASE="http://127.0.0.1:8080"

# via nginx reverse proxy (debe strippear /control)
# BASE="https://fluxbee.ai/control"
```

## 1) Health y estado local

```bash
curl -sS "$BASE/health"
curl -sS "$BASE/hive/status"
curl -sS "$BASE/hives"
```

Esperado:
- `{"status":"ok"}` en `/health`
- `status=ok` en `hive/status`
- lista JSON de hives en `/hives`

## 2) Alta/baja de hive (orchestrator)

```bash
# ajustar address al worker real
HIVE_ID="worker-test"
HIVE_ADDR="192.168.8.50"

curl -sS -X POST "$BASE/hives" \
  -H 'Content-Type: application/json' \
  -d "{\"hive_id\":\"$HIVE_ID\",\"address\":\"$HIVE_ADDR\"}"

curl -sS "$BASE/hives/$HIVE_ID"

# cleanup
curl -sS -X DELETE "$BASE/hives/$HIVE_ID"
```

Esperado:
- `status=ok` en alta
- `status=ok` + datos en get hive
- `status=ok` en delete
- `delete` detiene y deshabilita servicios Fluxbee remotos del worker (`rt-gateway`, `sy-config-routes`, `sy-opa-rules`, `sy-identity`) antes de borrar metadata local.

### 2.1 Matriz de errores add_hive (rápido)

Script automático:

```bash
bash scripts/admin_add_hive_matrix.sh
```

Casos cubiertos por script:
- `INVALID_ADDRESS` -> HTTP `400`
- `INVALID_HIVE_ID` -> HTTP `400`
- `HIVE_EXISTS` -> HTTP `409` (si existe al menos una hive)
- `SSH_*` -> HTTP `502/504`

Caso manual adicional:
- `WAN_NOT_AUTHORIZED` -> HTTP `409`
  - Cuando motherbee tiene `wan.authorized_hives` no vacio y falta el `hive_id` solicitado.
- `WAN_TIMEOUT` -> HTTP `504`
  - Requiere host remoto que bootstrappee pero no logre conexión WAN dentro de 60s.

## 3) Router remoto por hive (histórico/deprecado)

Contrato actual:
- `/hives/{id}/routers*` no existe en `SY.admin` (debe devolver `404`).
- El control canónico para entidades `RT.*` es vía `/hives/{id}/nodes` (`SPAWN_NODE`/`KILL_NODE`), sujeto a disponibilidad de runtime/contrato operativo del router.

## 4) Node remoto por hive

```bash
HIVE_ID="worker-test"

# ejemplo: runtime declarado en runtime-manifest.json
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H 'Content-Type: application/json' \
  -d '{"runtime":"wf.echo","version":"current"}'

curl -sS "$BASE/hives/$HIVE_ID/nodes"
```

Nota:
- `DELETE /hives/{id}/nodes/{name}` existe, pero para casos con unit generado automaticamente conviene validar primero el identificador operativo a matar.
- en hive remoto, los `name` listados deben pertenecer al hive target (ej: `SY.config.routes@worker-220`), no al motherbee.

## 5) Config storage (broadcast)

```bash
curl -sS "$BASE/config/storage"

curl -sS -X PUT "$BASE/config/storage" \
  -H 'Content-Type: application/json' \
  -d '{"path":"/var/lib/fluxbee/storage"}'
```

Esperado:
- `GET` devuelve `status=ok` y `path`
- `PUT` devuelve `status=ok`
- `version` es opcional; si se envia manual y no es mayor al actual, responde `409 VERSION_MISMATCH`

## 6) Config VPN (sanity)

```bash
curl -sS -X PUT "$BASE/config/vpns" \
  -H 'Content-Type: application/json' \
  -d '{"vpns":[
    {"pattern":"WF.echo","match_kind":"PREFIX","vpn_id":20},
    {"pattern":"WF.listen","match_kind":"PREFIX","vpn_id":20}
  ]}'
```

Esperado:
- `{"status":"ok"}`

## 7) Verificacion por logs

```bash
sudo journalctl -u sy-admin -n 120 --no-pager
sudo journalctl -u sy-orchestrator -n 120 --no-pager
```

Buscar:
- acciones admin recibidas en orchestrator
- respuestas `status=ok`/`status=error` con `error_code` cuando aplique

## 8) Nota Nginx

Si con proxy falla `/control/*` con `not_found`, revisar que el `location /control/` haga `proxy_pass` con `/` final para remover prefijo:

```nginx
location /control/ {
  proxy_pass http://127.0.0.1:8080/;
}
```

## 9) Sin legacy

Estos endpoints legacy ya no existen y deben devolver `404`:

```bash
curl -sS "$BASE/nodes"
curl -sS "$BASE/routers"
```

## 10) E2E script (batch)

Script automatizado para cubrir en una sola corrida:
- `GET/POST/DELETE /hives/{id}/nodes`
- `GET/PUT /config/storage` (incluye restore)

Estado actual:
- `scripts/admin_nodes_routers_storage_e2e.sh` es histórico (incluye `/routers*`).
- Para inventario y control multi-hive usar scripts de `system-inventory-spec` (`inventory_*_e2e.sh`).

Opcionales históricos:
- `RUN_ROUTER_CYCLE=0`
- `RUN_NODE_CYCLE=0`
- `NODE_RUNTIME=wf.echo NODE_VERSION=current`
- `SKIP_NODE_IF_RUNTIME_MISSING=1`

## 11) WAN stale/recovery E2E (router remoto, histórico/deprecado)

Script automatizado para validar transicion:
- `alive -> stale` (al detener router remoto)
- `stale -> alive` (al levantar router remoto)
- UUID remoto no-nil despues de recovery

El script `scripts/admin_wan_stale_recovery_e2e.sh` quedó histórico por dependencia de `/hives/{id}/routers*`.
Para stale/recovery de inventario ver `scripts/inventory_stale_hive_e2e.sh` (D3), pendiente de trigger canónico de stale sin `/routers*`.

Opcionales:
- `STALE_TIMEOUT_SECS=90` (default `90`)
- `RECOVERY_TIMEOUT_SECS=60` (default `60`)
- `POLL_INTERVAL_SECS=2`
- `EXPECTED_STALE_AFTER_SECS=30` (solo para logging de progreso; default `30`)
- `AUTO_RECOVER_ON_EXIT=1` (intenta levantar router si el script se interrumpe tras un kill)

## 12) WAN integration suite (add/remove + freshness gate + stale/recovery)

Suite de integración completa para cerrar flujo WAN de orchestrator:
- `add_hive` con gate de frescura (`payload.wan_connected=true`)
- transición `alive -> stale -> alive`
- `remove_hive` y validación `NOT_FOUND` posterior

```bash
BASE="http://127.0.0.1:8080" \
HIVE_ID="worker-220" \
HIVE_ADDR="192.168.8.220" \
bash scripts/admin_wan_integration_suite.sh
```

Opcionales:
- `STALE_TIMEOUT_SECS=120`
- `RECOVERY_TIMEOUT_SECS=60`
- `POLL_INTERVAL_SECS=2`
