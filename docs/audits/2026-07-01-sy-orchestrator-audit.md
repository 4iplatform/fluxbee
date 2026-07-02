# Auditoria sy.orchestrator - tercera pasada post-remediacion

Fecha: 2026-07-01  
Commit revisado: `fa08233` (`daily_onworking_coa`)  
Base: seguimiento de [`2026-06-30-sy-orchestrator-audit.md`](./2026-06-30-sy-orchestrator-audit.md), luego de marcar sus 11 items como resueltos.  
Foco: una mirada fresca sobre `src/bin/sy_orchestrator.rs` y el borde inmediato de `system_policy`, buscando regresiones o problemas nuevos introducidos por los fixes.

## Resumen ejecutivo

La segunda pasada no encontro regresiones en los fixes criticos de ownership de units, guard local de `SYSTEM`, locks por nodo, permisos 0600, ni en la idempotencia de nftables. `cargo test --bin sy_orchestrator` pasa con 109/109 tests.

Si aparecieron 3 temas nuevos/residuales:

1. **Media/Alta:** `add_hive` de workers todavia interpola paths de `blob/dist` en shell remoto y YAML con cobertura incompleta de quoting/validacion.
2. **Media:** `run_node` sigue dejando identity/ILK/mapping persistidos si falla despues del registro, en especial `CONFIG_WRITE_FAILED` y `SPAWN_FAILED`.
3. **Baja/Media:** el cleanup de SSH bootstrap fallido depende de `sudo -n`; en fallos de sudo setup puede quedar la key abierta aunque podria removerse como usuario normal.

## Hallazgos

### SO-2026-07-01-01 - Media/Alta - `add_hive` de workers interpola paths `blob/dist` en shell/YAML sin cobertura completa

Estado: Resuelto en remediacion 2026-07-02

Evidencia:

- `blob_runtime_from_hive` y `dist_runtime_from_hive` copian `blob.path`, `blob.sync.data_dir`, `blob.sync.tool`, `dist.path` y `dist.sync.tool` desde `hive.yaml` con `trim`/lowercase, pero sin validacion global de paths absolutos, control chars, quote/backslash o shell metachars (`src/bin/sy_orchestrator.rs:3379-3499`).
- En el provisioning de worker, el `mkdir -p` remoto interpola `state.blob.path`, `blob_active_dir`, `blob_staging_dir` y `state.blob.sync_data_dir` dentro de comillas simples manuales, sin `shell_single_quote` (`src/bin/sy_orchestrator.rs:16102-16113`).
- El mismo archivo ya tiene helper de quoteo (`shell_single_quote`, `src/bin/sy_orchestrator.rs:14340-14342`) y lo usa en otros comandos remotos, lo que deja este bloque como outlier.
- El loop B-3 de `validate_yaml_scalar` para worker valida `hive_id`, uplink, `storage.path` e identity, pero no valida `desired_blob.path`, `desired_blob.sync_tool`, `desired_blob.sync_data_dir`, `desired_dist.path` ni `desired_dist.sync_tool` (`src/bin/sy_orchestrator.rs:16175-16192`).
- Esos campos no validados se escriben en el `hive_yaml` del worker, varios dentro de escalares YAML con comillas dobles (`src/bin/sy_orchestrator.rs:16194-16213`).

Impacto:

En operacion normal estos valores son operator-owned, pero son entradas de configuracion que terminan como comando remoto root durante `add_hive`. Una comilla simple o caracter de control en un path puede romper el bootstrap; si alguien controla el `hive.yaml` local, tambien puede convertirlo en inyeccion shell sobre el worker. Ademas, un valor con `"`/`\`/newline en los campos omitidos puede generar un `hive.yaml` remoto malformado o semanticamente distinto.

Recomendacion:

- No construir el `hive.yaml` con `format!` para datos estructurados: renderizar con `serde_yaml` o un builder que escape escalares.
- Validar todos los campos de `BlobRuntimeConfig` y `DistRuntimeConfig` que entran a shell/YAML: paths absolutos, sin control chars, y con politica explicita para quote/backslash.
- Para comandos remotos, construir la lista de paths y aplicar `shell_single_quote` a cada argumento, o evitar shell cuando sea viable.
- Agregar tests con `blob.path`, `blob.sync.data_dir` y `dist.path` que contengan `'`, `"`, `\` y newline, verificando rechazo antes de SSH/render.

### SO-2026-07-01-02 - Media - `run_node` conserva identity/ILK si falla despues del registro

Estado: Resuelto en remediacion 2026-07-02

Evidencia:

- `ensure_node_identity_registered` registra en `SY.identity` y persiste `node_name -> ilk_id/tenant_id` localmente antes de que exista config o proceso (`src/bin/sy_orchestrator.rs:11395-11425`).
- `run_node_flow` hace register/update de identity y recien despues llama a `ensure_node_effective_config_on_spawn` (`src/bin/sy_orchestrator.rs:12815-12890`).
- Si falla la escritura del config, el flujo retorna `CONFIG_WRITE_FAILED` con el bloque `identity`, pero no revierte mapping ni ILK (`src/bin/sy_orchestrator.rs:12869-12890`).
- Si falla `systemd-run`, el rollback best-effort borra solo `config_path` (`src/bin/sy_orchestrator.rs:12947-12952`).
- La remocion de mapping/ILK existe en los caminos explicitos de teardown/purge (`src/bin/sy_orchestrator.rs:13526-13555`, `src/bin/sy_orchestrator.rs:13780-13830`), no como rollback automatico del spawn fallido.

Impacto:

El retry ya no queda bloqueado por `NODE_ALREADY_EXISTS`, que era el bug principal de la auditoria anterior. Pero todavia puede quedar un ILK/mapping para un nodo que nunca arranco, y si el payload pidio update de identity/canales, esa mutacion queda aplicada aunque el proceso no exista. Esto ensucia inventario/auditoria de identity y obliga a cleanup manual o a un retry que reutilice el estado parcial.

Recomendacion:

- Modelar el spawn como transaccion o estado explicito `pending_spawn`: reservar identity, escribir config, arrancar unidad y recien despues marcar identity activa.
- Si el ILK fue creado en este intento, hacer rollback de mapping y pedir delete del ILK en `CONFIG_WRITE_FAILED` y `SPAWN_FAILED`. Si el ILK ya existia, no borrarlo; marcar el caso como resume/retry.
- Agregar tests con falla inyectada de `write_json_atomic` y de `execute_on_hive` que verifiquen cleanup o estado `pending_spawn` observable.

### SO-2026-07-01-03 - Baja/Media - Cleanup de SSH bootstrap fallido depende de `sudo -n`

Estado: Resuelto en remediacion 2026-07-02

Evidencia:

- Worker y egress pueden sembrar la key de motherbee por password antes de configurar sudoers (`src/bin/sy_orchestrator.rs:15956-15967`, `src/bin/sy_orchestrator.rs:16757-16768`).
- Si falla `ensure_remote_orchestrator_sudoers_with_access`, el flujo retorna `SUDO_SETUP_FAILED` (`src/bin/sy_orchestrator.rs:15971-15981`, `src/bin/sy_orchestrator.rs:16770-16777`).
- El wrapper de admin agrega `ssh_bootstrap_open` en errores llamando `best_effort_revoke_bootstrap` (`src/bin/sy_orchestrator.rs:2047-2056`, `src/bin/sy_orchestrator.rs:2079-2089`).
- Ese best-effort llama a `revoke_bootstrap_ssh_access`, que envuelve todo el script en `sudo_wrap` (`src/bin/sy_orchestrator.rs:17839-17868`). Si el fallo fue justamente sudo setup, es probable que no pueda ejecutar el cleanup aunque la key permita login de usuario.
- La ruta de seeding por password edita `~/.ssh/authorized_keys` como usuario normal, sin sudo (`src/bin/sy_orchestrator.rs:17815-17825`), asi que existe un cleanup parcial posible que hoy no se intenta.

Impacto:

El fix actual al menos reporta `ssh_bootstrap_open=true`, por lo que no es silencioso. Aun asi, deja una key de login abierta en una clase de fallo donde podria limpiarse sin privilegios root. No necesariamente queda sudo/root, pero si queda acceso SSH al usuario bootstrap hasta limpieza manual.

Recomendacion:

- Separar el cleanup en dos fases: remover la entrada de `authorized_keys` como usuario remoto (por key y, si hay password, por password fallback), y luego remover sudoers solo si `sudo -n` funciona.
- Reportar flags separados, por ejemplo `ssh_key_removed`, `sudoers_removed`, `ssh_bootstrap_open`.
- Agregar test/unit seam para el caso `SUDO_SETUP_FAILED` post-key-seed.

## No regresiones observadas

- `run_node` y `kill_node` ya derivan la unit desde `node_name` y no aceptan units arbitrarias del caller (`src/bin/sy_orchestrator.rs:12656-12660`, `src/bin/sy_orchestrator.rs:13376-13390`).
- `handle_system_message` tiene guard defensivo local usando la politica compartida de `system_policy` (`src/bin/sy_orchestrator.rs:2148-2189`; `src/router/system_policy.rs:25-88`).
- Existen locks por `node_name` en config/lifecycle local y remoto (`src/bin/sy_orchestrator.rs:12293-12295`, `src/bin/sy_orchestrator.rs:12605-12608`, `src/bin/sy_orchestrator.rs:13222-13235`, `src/bin/sy_orchestrator.rs:13388-13418`, `src/bin/sy_orchestrator.rs:13704-13706`).
- `write_secret_file_0600` reasegura permisos despues de escribir, cubriendo archivos preexistentes con modo amplio (`src/bin/sy_orchestrator.rs:825-843`).
- La aplicacion nft `add table` + `flush table` fue verificada dos veces en un container descartable `fluxbee-lab:latest` con nftables v1.0.9: la segunda aplicacion no fallo. No se conserva el finding sospechado sobre SO-08.

## Verificacion ejecutada en la auditoria inicial

- `cargo test --bin sy_orchestrator`: **109 passed / 0 failed**.
- Check aislado en Docker: `nft -f` aplicado dos veces sobre una tabla de prueba con `add table` + `flush table`: **OK** (`nftables v1.0.9`).

## Gaps de la auditoria inicial

- No se ejecuto E2E real en Proxmox ni bootstrap completo de worker/egress durante esta pasada.
- No se probaron fallos inyectados de IO/sudo/SSH en un harness; los findings se derivan de lectura de flujo y evidencia estatica.
- No se hizo fuzzing de `hive.yaml` ni de payloads `add_hive`/`run_node`.

## Orden sugerido de remediacion original

1. Cerrar el renderer/quoting de `add_hive` worker (`blob/dist` en shell + YAML), porque combina impacto operativo con una superficie de inyeccion remota.
2. Definir semantica transaccional de identity para `run_node` fallido: rollback real o estado `pending_spawn`.
3. Mejorar el cleanup SSH de add_hive fallido con remocion user-level de `authorized_keys` antes de depender de sudo.

## Remediacion aplicada y verificacion empirica - 2026-07-02

Estado actual: los tres findings originales quedaron corregidos en codigo y se valido el flujo real `add_hive` de worker en Proxmox.

Cambios aplicados:

- **SO-2026-07-01-01:** se agrego validacion de `blob/dist` antes de renderizar/provisionar workers (`blob.path`, `blob.sync.data_dir`, `blob.sync.tool`, `dist.path`, `dist.sync.tool`) y se quotean los paths remotos con quoting shell por argumento.
- **SO-2026-07-01-02:** `run_node` ahora conserva `requested_ilk_id`, detecta cuando el ILK fue creado en el intento actual y hace rollback best-effort de mapping/ILK ante `CONFIG_WRITE_FAILED` o `SPAWN_FAILED`.
- **SO-2026-07-01-03:** el cleanup de bootstrap SSH ahora separa `authorized_keys` de sudoers, intenta remocion user-level por key/password fallback y reporta `ssh_key_removed`, `sudoers_removed` y `ssh_bootstrap_open`.

Hallazgos empiricos nuevos durante la remediacion:

- **SO-2026-07-02-01 - Resuelto - `sudo_wrap` no elevaba comandos compuestos completos.** En Proxmox, `add_hive` fallo distribuyendo la key HMAC de identity: `chmod: changing permissions of '/var/lib/fluxbee/identity/keys': Operation not permitted`. Causa: `sudo_wrap("mkdir ... && chmod ...")` ejecutaba solo el primer comando bajo sudo; el `chmod` corria como usuario bootstrap. Se agrego `sudo_bash_wrap` y se aplico al mkdir/chmod de TLS e identity HMAC.
- **SO-2026-07-02-02 - Resuelto - `fluxbee-firstboot` podia terminar sin hive ready.** En VM limpia con el paquete anterior, `vault_put` funciono pero `sy-storage` arranco degradado porque consulto vault durante una ventana de desconexion, y `sy-orchestrator` quedo reiniciando por `STORAGE_NOT_READY`. Se endurecio `packaging/fluxbee-firstboot` con `wait_hive_ready`, reintentos de reconexion vault/storage/orchestrator y salida no-cero si `/hives` no llega a `status:ok`.

Verificacion ejecutada:

- `cargo test --bin sy_orchestrator`: **114 passed / 0 failed**.
- `bash -n packaging/fluxbee-firstboot`: **OK**.
- Build Linux Docker con `fluxbee-lab:latest`: **OK**, paquete generado `dist/fluxbee_0.1.0-auditfix2_amd64.deb`.
- SHA256 del paquete `auditfix2`: `e27305c4eb629cda1883e021c9598c4f6cbe240df118bff0d0e005e4b3c2a03c`.
- Proxmox via `lab/pve.py` sobre nodo `sandbox`:
  - VM 211 `fb-audit-mb`: instalo `fluxbee 0.1.0-auditfix2`, servicios core activos, `/hives` devuelve `status:ok`.
  - VM 212 `fb-audit-wk`: worker vacio preparado con SSH/PostgreSQL.
  - `POST /hives` para `worker-audit` contra `192.168.103.146`: **OK**.
  - Respuesta `add_hive`: `wan_connected=true`, `orchestrator_connected=true`, `dist_sync_ready=true`, `syncthing_peer_linked=true`, `ssh_bootstrap_revoked=true`.
  - Inventory: `worker-audit` aparece `alive` con 8 nodos SY activos.
  - Worker: `rt-gateway`, `sy-orchestrator`, `sy-identity`, `sy-config-routes`, `sy-opa-rules`, `sy-cognition`, `sy-policy`, `sy-timer`, `sy-wf-rules` y `fluxbee-syncthing` activos.
  - Key HMAC remota: `/var/lib/fluxbee/identity/keys` queda `0700 root:root`; `/var/lib/fluxbee/identity/keys/worker-audit.key` queda `0600 root:root`.

Gaps remanentes:

- No se re-clono una tercera motherbee solo para probar `fluxbee-firstboot` de `auditfix2` desde cero; la carrera se reprodujo con el paquete previo, se corrigio el script, se valido sintaxis, se instalo `auditfix2` y se verifico que la version instalada contiene el helper corregido.
- No se ejecuto `run_node` E2E con una falla inyectada de `systemd-run`; la cobertura de rollback quedo en unidad/lectura de flujo.
