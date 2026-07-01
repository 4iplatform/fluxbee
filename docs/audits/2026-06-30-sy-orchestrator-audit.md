# Auditoria sy.orchestrator - vision fresca

Fecha: 2026-06-30
Repositorio: `/Users/cagostino/code/fluxbee`
Branch: `daily_onworking_coa`
Commit revisado: `25e883e`
Foco: `src/bin/sy_orchestrator.rs` y fronteras inmediatas de router/policy/identity que condicionan seguridad u operatividad.

## Resumen ejecutivo

`sy.orchestrator` mejoro claramente respecto de la auditoria del 2026-06-23: hay gate de origen para acciones admin mutantes, `hive_id` se valida antes de tocar paths destructivos, `add_hive/remove_hive` dejaron de usar SSH como fallback operativo cuando el worker ya debe responder por socket, `write_remote_file` ya no interpola contenidos en shell remoto, y egress valida nombres de interfaces antes de llegar a YAML/nft/shell.

La nueva superficie de codigo introduce, sin embargo, riesgos relevantes en lifecycle de nodos dinamicos y bootstrap de hives:

- El campo `unit` no esta atado al ownership de un nodo gestionado. `KILL_NODE` puede parar unidades arbitrarias de systemd y `SPAWN_NODE` puede registrar/configurar nodos contra una unidad elegida por el caller.
- `add_hive` deja acceso SSH de bootstrap con capacidad root por defecto, y tambien durante fallos parciales previos al paso de hardening.
- `run_node` muta `SY.identity` y el mapping local antes de comprobar si ya existe config local y antes de confirmar que `systemd-run` funciono, dejando estados parciales dificiles de reconciliar.
- No hay lock por nodo para serializar `run_node/start/restart/kill/remove/config_set`; el archivo de config atomico no alcanza para proteger los efectos en identity, systemd y timers.
- La autoridad `SYSTEM` esta centralizada en router, pero `sy.orchestrator` no mantiene un guard defensivo local para acciones protegidas si una ruta de entrega futura/bypass/test evita ese filtro.

Validacion local ejecutada:

- `cargo test --bin sy_orchestrator`: OK, 106 tests passed, 0 failed. El build emitio warnings preexistentes de `dead_code`/`unused`, sin fallos.

## Hallazgos

### SO-2026-06-30-01 - Alta - `unit` permite controlar unidades systemd fuera del nodo gestionado

Estado: Abierto

Evidencia:

- `run_node_flow` acepta `payload.unit`, lo sanitiza pero lo usa como unidad final si esta presente (`src/bin/sy_orchestrator.rs:12518-12523`).
- El comando de spawn usa ese valor en `systemd-run --unit ...` (`src/bin/sy_orchestrator.rs:14157-14189`).
- `kill_node_flow` permite omitir `node_name` y usar `payload.unit`; tambien solo sanitiza caracteres (`src/bin/sy_orchestrator.rs:13197-13244`).
- Luego ejecuta `systemctl kill/stop/reset-failed {unit}` (`src/bin/sy_orchestrator.rs:13264-13270`, `src/bin/sy_orchestrator.rs:13339-13340`).
- `KILL_NODE` y `SPAWN_NODE` estan en la tabla de acciones `SYSTEM` protegidas (`src/router/system_policy.rs:25-43`), pero una vez autorizado el caller, no hay validacion de que la unidad pertenezca al nodo.

Impacto:

Un caller autorizado para lifecycle de nodos podria parar servicios core del host (`sy-orchestrator`, `sy-identity`, `rt-gateway`, `ssh`, `postgresql`, etc.) enviando `KILL_NODE` con `unit` directo. Esto saltea los invariantes de lifecycle administrado y puede producir DoS local o cross-hive, porque `SY.orchestrator@<any hive>` esta autorizado para acciones protegidas (`src/router/system_policy.rs:69-87`).

Recomendacion:

- Para la API publica/admin/system, exigir `node_name` y derivar siempre `unit` con `unit_from_node_name`.
- Si se conserva `unit`, permitir solo `fluxbee-node-*` y comprobar que corresponde a un config gestionado existente antes de operar.
- Separar control de servicios core en una accion explicita con allowlist reducida, sin autoridad cross-hive generica.
- Agregar tests para `KILL_NODE { unit: "sy-orchestrator" }`, `KILL_NODE` sin `node_name`, y `SPAWN_NODE` con `unit` colisionante.

### SO-2026-06-30-02 - Alta - Bootstrap SSH queda abierto por defecto y en fallos parciales

Estado: Abierto

Evidencia:

- `harden_ssh` default es `false` (`src/bin/sy_orchestrator.rs:17040-17045`).
- `restrict_ssh` default tambien queda `false` si `harden_ssh` no esta activo (`src/bin/sy_orchestrator.rs:17129-17142`).
- El bootstrap seedea la key por password como entrada sin `from=` ni forced command (`src/bin/sy_orchestrator.rs:15784-15823`, `src/bin/sy_orchestrator.rs:17579-17604`).
- El sudoers remoto concede `NOPASSWD` a varias herramientas root, incluyendo `/bin/bash` y `/usr/bin/bash` (`src/bin/sy_orchestrator.rs:17607-17611`).
- Los controles de SSH se aplican recien despues del finalize (`src/bin/sy_orchestrator.rs:16385-16420`, `src/bin/sy_orchestrator.rs:16854-16888`).
- Hay retornos tempranos luego de key/sudoers y antes del cierre: timeouts WAN/orchestrator/finalize en worker (`src/bin/sy_orchestrator.rs:16239-16311`) y egress (`src/bin/sy_orchestrator.rs:16818-16847`).
- Si `restrict_ssh` no fue solicitado, el codigo registra que deja la bootstrap key sin restriccion (`src/bin/sy_orchestrator.rs:17367-17371`).

Impacto:

En un `add_hive` exitoso con defaults, la key de motherbee queda con acceso SSH al worker/egress y escalacion root via sudoers. En un fallo parcial posterior a la siembra de clave/sudoers, el host puede quedar parcialmente gestionado con el mismo acceso abierto. En egress esto es especialmente sensible porque es un host de borde.

Recomendacion:

- Cambiar default a `harden_ssh=true` y `restrict_ssh=true`, o exigir opt-out explicito y auditado.
- Mejor aun: remover key y sudoers de bootstrap al finalizar correctamente si el contrato v2 dice que SSH es solo bootstrap.
- Si se necesita break-glass, instalar una entrada `authorized_keys` con `from=`, sin pty/forwarding, y forced command de allowlist pequena.
- En cada retorno posterior a key/sudoers, intentar cleanup best-effort o marcar estado pendiente con un flag visible tipo `ssh_bootstrap_open=true`.

### SO-2026-06-30-03 - Alta/Media - `run_node` muta identity antes de saber si el nodo puede crearse

Estado: Abierto

Evidencia:

- `run_node_flow` registra identidad antes de calcular/comprobar `config_path` (`src/bin/sy_orchestrator.rs:12627-12648`).
- Despues aplica update de identity si hay cambios (`src/bin/sy_orchestrator.rs:12655-12689`).
- Recien luego revisa si el config local ya existe y puede devolver `NODE_ALREADY_EXISTS` con identity ya mutada (`src/bin/sy_orchestrator.rs:12697-12724`).
- La funcion de registro llama a `SY.identity` y persiste el mapping local (`src/bin/sy_orchestrator.rs:11246-11308`).
- `SY.identity` actualiza/crea el ILK canonico por `identification.node_name` (`src/bin/sy_identity.rs:885-944`).
- Si la escritura de config o `systemd-run` falla, la respuesta vuelve error pero los efectos previos quedan (`src/bin/sy_orchestrator.rs:12730-12752`, `src/bin/sy_orchestrator.rs:12794-12817`).

Impacto:

Una repeticion de `run_node` contra un nodo existente puede modificar identity/mapping y terminar en `NODE_ALREADY_EXISTS`. Un fallo de `systemd-run` puede dejar config + ILK + mapping persistidos sin proceso corriendo; el retry posterior choca con el config existente y no reanuda de forma limpia.

Recomendacion:

- Hacer la comprobacion de `config_path.exists()` antes de cualquier mutacion en `SY.identity`.
- Convertir spawn en una transaccion o workflow reanudable: preparar config temporal, registrar identity, ejecutar `systemd-run`, y hacer commit final; si falla, rollback de config/mapping/ILK o estado `pending_spawn` con retry explicito.
- Agregar tests que verifiquen que `NODE_ALREADY_EXISTS` no dispara RPC a identity y que un `systemd-run` fallido no deja un estado que bloquee reintentos.

### SO-2026-06-30-04 - Media - Falta lock por nodo para lifecycle y config

Estado: Abierto

Evidencia:

- `OrchestratorState` tiene locks para runtime y topologia de hives, pero no un lock por nodo gestionado (`src/bin/sy_orchestrator.rs:466-498`).
- El lock por hive se aplica a add/remove hive (`src/bin/sy_orchestrator.rs:513-525`, `src/bin/sy_orchestrator.rs:15434-15437`, `src/bin/sy_orchestrator.rs:8022-8024`).
- La creacion de config de nodo hace check `path.exists()` y luego `write_json_atomic`, pero sin guard compartido con identity/systemd/kill/remove (`src/bin/sy_orchestrator.rs:9499-9520`).
- `run_node_flow` ejecuta identity, config y systemd dentro de una ventana larga sin lock de nodo (`src/bin/sy_orchestrator.rs:12627-12817`).

Impacto:

Dos `run_node` simultaneos para el mismo nodo pueden registrar/updatear identity en ambos caminos y competir por el config. Un `kill_node purge_instance` o `remove_node_instance` puede borrar estado mientras `run_node` esta creando config o arrancando unidad. El resultado probable es drift entre identity, config local, timers y systemd.

Recomendacion:

- Agregar un registry de locks por `node_name` normalizado, similar a `hive_topology_locks`.
- Mantener el lock durante `run_node`, `start_node`, `restart_node`, `kill_node`, `remove_node_instance` y `NODE_CONFIG_SET` para el nodo local.
- Cubrir con tests concurrentes para `run_node/run_node` y `run_node/kill_node purge_instance`.

### SO-2026-06-30-05 - Media - `SYSTEM` depende solo del router para acciones protegidas

Estado: Abierto

Evidencia:

- El perfil RPC entrega cualquier `SYSTEM` post-pending al command channel del orchestrator (`src/bin/sy_orchestrator.rs:528-546`).
- `handle_system_message` declara que no revalida origen porque router lo filtra centralmente (`src/bin/sy_orchestrator.rs:2091-2099`).
- Si un mensaje llega a ese handler, ejecuta acciones protegidas como `SPAWN_NODE`, `KILL_NODE`, `NODE_CONFIG_SET`, `ADD_HIVE_FINALIZE` y `REMOVE_HIVE_CLEANUP` (`src/bin/sy_orchestrator.rs:2119-2207`).
- El router filtra actualmente antes de entrega local (`src/router/mod.rs:5628-5643`).

Impacto:

El diseno centralizado es razonable en ruta normal, pero el receptor queda sin defensa local frente a un bypass de test, una version vieja de router, una ruta alternativa futura, o una regresion en `serialize_for_local_delivery`. Si esa frontera falla, el impacto es total porque el handler ejecuta mutaciones criticas.

Recomendacion:

- Mantener router como fuente de verdad, pero agregar un guard defensivo en `sy.orchestrator` para acciones `SYSTEM` protegidas usando la misma politica compartida o un helper expuesto.
- Como minimo, rechazar acciones protegidas si `msg.routing.src_l2_name` es `None` y registrar alerta.
- Agregar test que invoque `handle_system_message` con una accion protegida y origen no autorizado, sin pasar por router.

### SO-2026-06-30-06 - Media/Baja - Campos de `hive.yaml` entran crudos en unit file de syncthing

Estado: Abierto

Evidencia:

- `blob_runtime_from_hive` copia `blob.path`, `blob.sync.data_dir` y `blob.sync.service_user` desde YAML con trim, pero sin validacion de caracteres/control chars ni normalizacion de paths (`src/bin/sy_orchestrator.rs:3287-3335`).
- `resolve_syncthing_service_user` verifica que el usuario exista o cae a root si se habilito fallback, pero no valida el valor para uso en unit files (`src/bin/sy_orchestrator.rs:3730-3748`).
- `syncthing_unit_contents` interpola `User=`, `Group=`, `WorkingDirectory=`, `Environment=HOME=` y `ExecStart --home=` con strings crudos (`src/bin/sy_orchestrator.rs:4774-4785`).
- Luego escribe `/etc/systemd/system/fluxbee-syncthing.service` como root (`src/bin/sy_orchestrator.rs:4789-4801`).

Impacto:

No es shell injection directa, pero un path con newline, `%`/specifier raro o caracteres no esperados puede generar un unit file invalido o inyectar directivas systemd si alguien controla `hive.yaml`. En el caso normal el config es operator-owned, por eso la severidad es menor; como hardening de operatividad conviene cerrar la clase.

Recomendacion:

- Rechazar control chars/newlines y exigir paths absolutos normalizados para `blob.path`, `dist.path` y `blob.sync.data_dir`.
- Usar escaping correcto para systemd (`systemd-escape`/reglas equivalentes) o escribir unit fields via una representacion validada.
- Agregar tests con `sync.data_dir` que contiene newline, espacios y `%` para confirmar rechazo o escape estable.

### SO-2026-06-30-07 - Media/Baja - `write_secret_file_0600` no repara permisos de archivos existentes

Estado: Abierto

Evidencia:

- `write_secret_file_0600` abre con `.create(true).truncate(true).mode(0o600)` y escribe, pero no hace `set_permissions` despues (`src/bin/sy_orchestrator.rs:800-813`).
- `.mode(0o600)` solo aplica si el archivo se crea. Si el archivo existia con permisos amplios, el truncate conserva el modo anterior.
- Se usa para la key TLS local del hive (`src/bin/sy_orchestrator.rs:873-884`).

Impacto:

Si `cert.key` quedo alguna vez con `0644` u otro modo amplio, una reescritura/rotacion posterior no corrige el permiso y mantiene la exposicion local.

Recomendacion:

- Hacer `fs::set_permissions(path, 0o600)` despues de escribir, o escribir a temp 0600 y renombrar atomicamente.
- Agregar test con archivo preexistente `0644`.

### SO-2026-06-30-08 - Baja/Media - Egress reporta sysctl como OK sin readback y tiene ventana delete-then-apply en nft

Estado: Abierto

Evidencia:

- `reconcile_egress_nat` corre `apply_sysctl_system()` y marca `ipv4_forwarding=true` e `ipv6_blocked=true` sin leer de vuelta los valores reales del kernel (`src/bin/sy_orchestrator.rs:4562-4569`).
- En nftables borra la tabla existente y luego aplica el archivo nuevo (`src/bin/sy_orchestrator.rs:4584-4595`).
- Conntrack si tiene readback/estado real (`src/bin/sy_orchestrator.rs:4580-4582`), lo cual muestra el patron que falta para sysctl.

Impacto:

La verificacion puede reportar un egress sano aunque otro proceso/politica haya rechazado o revertido sysctl. Durante una reconciliacion hay una ventana corta sin la tabla `fluxbee_egress`; con forwarding activo, el plano de borde queda temporalmente sin esa politica.

Recomendacion:

- Leer de vuelta `net.ipv4.ip_forward` y los sysctl IPv6 relevantes despues de aplicar.
- Evitar delete-then-apply: usar una transaccion nft que reemplace/flush los objetos dentro de `nft -f` sin dejar gap observable, o staging table + swap equivalente.
- Agregar test unitario del verificador de sysctl y, si es posible, un test de integracion en namespace de red.

## No regresiones observadas de la auditoria anterior

Esta pasada no encontro regresion en los arreglos mas criticos del documento del 2026-06-23:

- Admin mutante exige origen `SY.admin@<hive local>` (`src/bin/sy_orchestrator.rs:1789-1811`, `src/bin/sy_orchestrator.rs:2221-2253`).
- `hive_id` se valida temprano y hay guard lexical para paths de hive (`src/bin/sy_orchestrator.rs:16987-17004`, `src/bin/sy_orchestrator.rs:15417-15437`).
- `remove_hive` usa socket o local-only, sin fallback SSH operativo (`src/bin/sy_orchestrator.rs:8062-8111`).
- `write_remote_file` transmite contenidos por stdin a `sudo tee`, sin heredoc/interpolacion de payload (`src/bin/sy_orchestrator.rs:17937-18005`).
- Egress valida nombres de interfaz con allowlist antes de sinks shell/nft/YAML (`src/bin/sy_orchestrator.rs:4161-4215`).

## Gaps de la auditoria

- No se ejecuto E2E con hives reales, egress real, ni pruebas de red/iptables/nft en namespace.
- No se ejercito una topologia multi-hive viva con routers de versiones mixtas.
- No se hizo fuzzing de payloads admin/system.
- La revision fue estatica + unit tests locales; los findings de concurrencia deben confirmarse con pruebas dirigidas, pero la ausencia de lock por nodo y el orden de side effects son visibles en codigo.

## Orden sugerido de remediacion

1. Cerrar ownership de `unit` en `run_node`/`kill_node` y cubrir con tests negativos.
2. Cambiar defaults/semantica de SSH bootstrap: cerrar o remover key/sudoers al final, y limpiar en fallos parciales.
3. Reordenar `run_node` para evitar side effects de identity antes del preflight local y definir rollback/retry para spawn fallido.
4. Agregar lock por nodo para lifecycle/config.
5. Agregar guard defensivo local para acciones `SYSTEM` protegidas.
6. Endurecer unit file de syncthing, permisos de secrets y readback/atomicidad de egress.

## Estado de remediacion (2026-07-01)

Verificacion previa: los 8 findings fueron confirmados contra el codigo actual (ninguno stale/falso); ademas se hallaron 3 temas nuevos (B-1/B-2/B-3, abajo).

| ID | Severidad | Estado | Commit |
| --- | --- | --- | --- |
| SO-2026-06-30-01 | Alta | **Resuelto** — `unit` siempre derivado de `node_name` (`fluxbee-node-*`); KILL_NODE exige `node_name`. No puede tocar units del host. | `0f0400a` |
| SO-2026-06-30-02 | Alta | **Abierto** — requiere cambio de default (`harden_ssh=true`) + cleanup en early returns (con B-2) + re-validacion 2-VM en lab antes de mergear. | — |
| SO-2026-06-30-03 | Alta/Media | **Resuelto** — preflight de config antes de mutar identity; rollback best-effort del config en `SPAWN_FAILED`. | `0f0400a` |
| SO-2026-06-30-04 | Media | **Resuelto** — lock async por `node_name` en run/kill/start/restart/remove/config_set. | `57b9e71` |
| SO-2026-06-30-05 | Media | **Resuelto** — guard defensivo local en `handle_system_message` con la misma `system_policy` (ahora `pub`). | `0f0400a` |
| SO-2026-06-30-06 | Media/Baja | **Resuelto** — validacion de control-chars/path-absoluto antes de escribir el unit de syncthing. | `cec4195` |
| SO-2026-06-30-07 | Media/Baja | **Resuelto** — `write_secret_file_0600` re-asegura 0600 post-write. | `0f0400a` |
| SO-2026-06-30-08 | Baja/Media | **Resuelto** — readback real de sysctl; nft add+flush+define atomico (sin delete-then-apply). | `cec4195` |
| B-1 (nuevo) | Baja | **Resuelto** — `ssh_user` validado por charset antes de tocar shell/ssh. | `cec4195` |
| B-2 (nuevo) | Baja | **Abierto** — mismo cleanup-on-early-return que SO-02 (la distribucion de identity-key agrega otro early-return fatal en la ventana SSH-abierta). | — |
| B-3 (nuevo) | Info | **Diferido** — valores del worker `hive.yaml` van por `tee` (sin shell injection); son derivados internos, riesgo muy bajo. | — |

Resueltos: 8/11 (los 3 Altas menos SO-02, todos los Media/Baja menos el SSH). Pendiente clave: **SO-02 + B-2** (SSH bootstrap), que se trata aparte porque cambia semantica de default y necesita re-validacion end-to-end del join en el lab Proxmox. Tests: `cargo test --bin sy_orchestrator` 108 passed / 0 failed en cada batch.
