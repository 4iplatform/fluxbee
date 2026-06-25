# Auditoria sy.orchestrator

Fecha: 2026-06-23
Commit auditado: `78428c5`
Branch: `daily_onworking_coa`
Archivo principal: `src/bin/sy_orchestrator.rs` (20562 lineas)

## Estado de resolucion (actualizado 2026-06-25)

Reconciliacion del trabajo posterior a la auditoria. Prioridad acordada con el operador: **operativos primero, seguridad despues** — la red es interna con puertas definidas y SSH se usa solo para el bootstrap inicial. Por eso el lote operativo + SSH-only-bootstrap se resolvio primero; el lote de seguridad (inyeccion de iface, gates de origen, allowlist) queda pendiente **por decision, no por descuido**.

**Resueltos (6 de 24):**

- **F1** remove_hive socket-only — SSH operativo eliminado; online -> `socket_ok/socket`, offline -> `local_only/local_only`, sin fallback SSH. *Validado empiricamente (lab, online + offline).*
- **F2** add_hive sin fallback SSH — `WORKER_SOCKET_UNREACHABLE` retryable cuando el worker esta online; clasificador alineado a los strings reales de `RpcError`. *Validado empiricamente (lab).*
- **F9** add_hive idempotente — reintento sobre `pending` resume via fast-path socket; `connected` sigue dando `HIVE_EXISTS`. *Validado empiricamente (lab) + unit test.*
- **F10** egress hardening fatal — el `Err` arm retorna `SSH_HARDEN_FAILED`/`SSH_KEY_FAILED`; info.yaml queda `pending` (reanudable) en fallo. *Codigo + revision adversarial (SOUND); falta validacion empirica (necesita rol egress, Fase 2 del lab).*
- **F14** watchdog panic-safe — `WatchdogRunGuard` (Drop) resetea el flag aun ante panic. *Unit test.*
- **F20** clamp de slices SHM — `shm_name_to_string` centraliza y clampa los 7 sitios. *Unit test.*

Ademas (no era finding del audit; cambio de diseno del operador): **las credenciales SSH del bootstrap se movieron al payload de `add_hive`** (`ssh_user` requerido / `ssh_password` opcional, probe key-first), eliminando los `administrator`/`magicAI` hardcodeados; el secreto se redacta en SY.architect; el schema de admin se actualizo. *Validado empiricamente (lab: worker bootstrapeado con creds del payload).*

**Pendientes (18 de 24)** — por severidad:

- **Alta (5):** **F7** (RCE root via iface sin validar — *el #1 del orden sugerido, SIGUE ABIERTO*), F8 (ADMIN_COMMAND sin gate de origen), F3 (traversal de `hive_id`), F4 (allowlist `system` no configurable), F11 (remove_hive egress no des-aprovisiona NAT — *tiene angulo operativo: router huerfano post-reboot*).
- **Media (8):** F12 + F13 (inyeccion nft / YAML — **mismo fix de iface que F7**), F5 (egress reporta `ok` incompleto), F6 (drift egress), F15 (allowlist cross-hive), F16 (conntrack_tuned), F17 (nft no carga al boot), F18 (TOCTOU sin lock).
- **Baja (5):** F19, F21, F22, F23, F25.

**Nota de seguridad (importante):** **F7** (con sus hermanos F12/F13) es el pendiente de mayor prioridad. El lote de creds reescribio el *threading* de `write_remote_file` (agrego el parametro `user`) pero **no** corrigio la construccion de shell por interpolacion ni agrego la allowlist de nombre de interfaz — el vector de F7 sigue abierto. Bajo el modelo interno+gated es motherbee/admin-autenticado (no RCE remoto no-autenticado), pero es un cruce real string-de-config -> shell como root.

Estado de codigo: los fixes resueltos viven en `src/` **sin commitear aun** (commit aparte del lab). Validacion: ver `lab/STATUS.md` (entorno containerizado donde F1/F2/F9 + creds se probaron sobre una malla real de 2 hives).

## Alcance

Se reviso `sy.orchestrator` con foco en las funcionalidades recientes: contrato v2 socket-first, `add_hive`/`remove_hive`, hardening de origen para acciones `system`, y soporte `role=egress` / NAT. El diff completo contra `main` es amplio; esta auditoria prioriza el comportamiento de `src/bin/sy_orchestrator.rs` y los contratos documentados alrededor de ese binario.

Metodo: revision adversarial multi-agente. Cada finding existente fue verificado contra el codigo real (numeros de linea confirmados en el commit `78428c5`); ademas se hizo una caceria por lentes (add_hive worker/egress, remove_hive, transporte SSH, concurrencia, autorizacion de origen, egress NAT, conformidad de contrato) y cada candidato nuevo se verifico adversarialmente. Tres findings se reprodujeron empiricamente (path traversal de `hive_id`, inyeccion de comandos en `write_remote_file`, inyeccion de reglas nft).

Comandos ejecutados:

```bash
cargo test --bin sy_orchestrator
bash scripts/router_dispatcher_guards/run_all.sh
```

Resultado:

- `cargo test --bin sy_orchestrator`: OK, 100 tests passed.
- `router_dispatcher_guards`: OK, 7/7 guards passed.

Importante sobre la cobertura de esos comandos: **ninguno de los findings de abajo es ejercitado por la suite actual**. Todos viven en caminos remotos/operativos (hives reales, SSH, nftables, dos NICs) que los tests unitarios y los guards no cubren. La verificacion de cada finding es por lectura de codigo (y, en tres casos, reproduccion empirica del quoting/FS), no por ejecucion de la suite. No se ejecutaron E2E con infraestructura real.

## Resumen ejecutivo

El binario compila y sus tests unitarios pasan, pero hay defectos de seguridad y de contrato relevantes. Los dos mas graves no estaban en la primera version de esta auditoria:

- **Inyeccion de comandos como root** en `write_remote_file` via `wan_iface`/`lan_iface` sin validar (F7, antes clasificado como "baja por comillas simples").
- **El canal `ADMIN_COMMAND` no consulta ninguna allowlist de origen** para acciones mutantes destructivas (F8).

Ademas:

- `remove_hive` y `add_hive` reintroducen/conservan SSH operativo contra el contrato v2 socket-first (F1, F2), contradiciendo items del backlog marcados como cerrados.
- `add_hive` valida `hive_id` despues de borrar directorios (F3) y no toma ningun lock (F18 TOCTOU).
- `add_hive` no es idempotente: un timeout deja el hive `pending` sin via de reintento (F9).
- El boundary egress puede quedar con SSH sin endurecer pero reportar `ok` (F10), y `remove_hive` de un egress deja el NAT huerfano (F11).
- La allowlist de origen `system` tiene prefijos hardcodeados que la vuelven no configurable (F4) y aceptan sufijo de hive arbitrario (F15).
- La verificacion egress sobre-afirma estado (F5, F16) y la tabla nft no se carga al boot (F17).

Tambien hay tres bugs de las funcionalidades nuevas comparten una sola causa raiz: **ausencia de allowlist de nombre de interfaz** (`wan_iface`/`lan_iface`). Un unico fix cierra F7 (RCE), F12 (inyeccion nft) y F13 (corrupcion YAML).

## Tabla resumen

| ID | Sev | Area | Titulo | Verificacion | Backlog |
|----|-----|------|--------|--------------|---------|
| F7  | Alta | ssh-transport | RCE como root en `write_remote_file` (iface sin validar) | empirica | nuevo (antes Baja) |
| F8  | Alta | origin-auth | `ADMIN_COMMAND` sin gate de origen | lectura | nuevo |
| F1  | Alta | remove_hive | SSH operativo en `remove_hive` | lectura+git | contradice `[x]` v2:21 |
| F2  | Alta | add_hive | `add_hive` socket-only cae a bootstrap SSH | lectura | contradice `[x]` v2:22 |
| F3  | Alta | add_hive | Borra dir antes de validar `hive_id` (traversal) | empirica | nuevo (antes Critica) |
| F4  | Alta | origin-auth | Allowlist `system` no configurable | lectura+test | parcial v2 |
| F9  | Alta | add_hive | `add_hive` no idempotente (`pending` atascado) | lectura | nuevo |
| F10 | Alta | add_hive | Hardening SSH egress no-fatal, reporta `ok` | lectura | nuevo |
| F11 | Alta | remove_hive | `remove_hive` egress no des-aprovisiona NAT | lectura | nuevo |
| F12 | Media | egress-nat | Inyeccion de reglas nft (iface sin validar) | empirica | nuevo |
| F13 | Media | ssh-transport | Corrupcion/inyeccion YAML en `hive.yaml` remoto | empirica | nuevo |
| F5  | Media | egress-nat | Egress reporta `ok` con verificacion incompleta | lectura | reescrito |
| F6  | Media | egress-nat | Watchdog egress no corrige drift parcial | lectura | decision CR-5 |
| F14 | Media | robustez | `watchdog_tick` no es panic-safe | lectura | nuevo |
| F15 | Media | origin-auth | Allowlist acepta sufijo de hive arbitrario (cross-hive) | lectura | nuevo (extiende F4) |
| F16 | Media | egress-nat | `egress_conntrack_tuned=true` incondicional | lectura | nuevo |
| F17 | Media | egress-nat | Tabla nft no se carga al boot | lectura | nuevo |
| F18 | Media | concurrency | TOCTOU: `add_hive`/`remove_hive` sin lock | lectura | nuevo |
| F19 | Baja | inventory | `list_hives` etiqueta egress como `worker` | lectura | nuevo |
| F20 | Baja | robustez | Slices de nombre SHM sin clamp (`node_name`) | lectura | nuevo |
| F21 | Baja | egress-nat | Host egress sin filtrado inbound | lectura | trade-off T-NET-4 |
| F22 | Baja | add_hive | Egress sin fast-path socket (re-provision re-corre SSH) | lectura | nuevo |
| F23 | Baja | egress-nat | `ipv4_forwarding`/`ipv6_blocked` sin read-back | lectura | nuevo |
| F25 | Baja | egress-nat | nft delete-then-apply no atomico | lectura | nuevo |

Severidades: 9 Alta, 9 Media, 6 Baja. (F24 quedo absorbido en F6; ver descartados al final para los falsos positivos.)

## Findings - Alta

### F7 - Alta - Inyeccion de comandos como root en `write_remote_file`

Verificacion: **empirica** (se reconstruyo la cadena de quoting exacta y se ejecuto contra bash real).

Reescrito desde la version anterior, que lo clasificaba como "Baja - puede alterar contenidos con comillas simples". La sospecha del propio finding anterior ("OJO IMPORTANTE") se confirmo: no es una cuestion de fidelidad byte, es ejecucion de comandos arbitrarios como root.

Evidencia:

- `write_remote_file` (`src/bin/sy_orchestrator.rs:16903-16906`):
  - `let escaped = contents.replace('\'', "'\"'\"'");`
  - `let cmd = format!("cat > {} <<'EOF'\n{}\nEOF", remote_path, escaped);`
  - `let sudo_cmd = sudo_wrap(&format!("bash -lc \"{}\"", cmd.replace('"', "\\\"")));`
- `sudo_wrap` (`src/bin/sy_orchestrator.rs:17107`) genera `sudo -n bash -lc "..."` (root). El cuerpo del heredoc viaja dentro de unas dobles comillas que el login shell remoto expande **antes** de procesar el `<<'EOF'`. El unico escape de esa capa exterior es `replace('"', "\\\"")`; nunca se escapan `$`, backtick ni `\`.
- Reproducido: con `contents` = `x=$(touch PWNED && echo INJECTED)` se crea el archivo `PWNED` en el host y el archivo escrito queda `x=INJECTED`. La variante con backticks (`y=` seguido de `touch PWNED2` entre acentos graves) tambien ejecuta. `z=$HOME` se expande.
- Fuente sin validar: `wan_iface`/`lan_iface` llegan del payload de `add_hive` y `resolve_egress_nat_config` (`src/bin/sy_orchestrator.rs:3801-3802`) solo hace `require_field` (= `trim` + no-vacio, `:3773-3779`). No hay ninguna validacion de nombre de interfaz en el repo. Se interpolan en `hive_yaml` (`add_egress_hive_flow:15841-15851`) y se pasan a `write_remote_file(/etc/fluxbee/hive.yaml, hive_yaml)` (`:15853`).
- Existe ya el primitivo correcto `shell_single_quote` (`src/bin/sy_orchestrator.rs:13598`), usado bien en otros sitios (p.ej. `:7378`); esta ruta lo ignora.

Impacto:

Un `add_hive role=egress` (motherbee-only, autenticado por admin) con `egress.wan_iface = "eth0$(...)"` ejecuta codigo arbitrario como root en el host egress remoto. Es un cruce de limite de confianza string-de-config -> shell, no mera corrupcion de archivo.

Mitigantes (por eso Alta y no Critica): `add_hive` es motherbee-only (`:1571`) y requiere un `ADMIN_COMMAND` autenticado; no es RCE no autenticado desde internet. El campo `egress.gateway_ip` si se valida como `Ipv4Addr` antes de interpolar (`render_worker_egress_yaml:3222-3224`), pero `wan_iface`/`lan_iface` (y otros strings libres como nombres de nodo identity, paths) no.

Recomendacion:

1. Validar `wan_iface`/`lan_iface` contra el formato real de interfaz Linux (`^[A-Za-z0-9._-]{1,15}$`, IFNAMSIZ<=15) en `resolve_egress_nat_config`, antes de cualquier interpolacion.
2. Reescribir `write_remote_file` para no construir shell por interpolacion: pasar `contents` por stdin a `ssh ... sudo tee <path>` (o `scp`, ya existe `scp_with_key:16855`), sin la capa envolvente `bash -lc "..."`.

### F8 - Alta - `ADMIN_COMMAND` no consulta allowlist de origen para acciones destructivas

Verificacion: lectura.

Evidencia:

- `handle_admin` (`src/bin/sy_orchestrator.rs:1409`) entra directo de `msg.meta.action` al match de acciones. `kill_node` (`:1511`), `set_node_config` (`:1507`), `run_node` (`:1510`), `restart_node` (`:1509`), `remove_node_instance` (`:1499`) no tienen ninguna verificacion de origen. `add_hive` (`:1571`) y `remove_hive` (`:1546`) solo chequean `state.is_motherbee` (propiedad del nodo local, no del emisor).
- El gate `is_allowed_system_source_name` solo se invoca en `handle_system_message` (`:1689`), nunca en `handle_admin` (grep confirma una sola aparicion).
- El canal ADMIN es alcanzable desde la malla: `pre_pending_rule(RouteMatch::exact(ADMIN_KIND, MSG_ADMIN_COMMAND), RouteTarget::Command(RPC_CH_ADMIN))` (`:508-511`) -> `run_admin_worker` -> `handle_admin` sin filtro.
- La misma `kill_node_flow` es alcanzable por dos caminos: `KILL_NODE` (system, gateado en `:1728`) y `kill_node` (admin, sin gatear en `:1511`).
- Agravante: `run_node` por ADMIN reenvia `SPAWN_NODE` al hive destino; el `src_l2_name` del mensaje reenviado es el del orchestrator relay, que siempre matchea `is_allowed_system_source_name` por `starts_with("SY.orchestrator@")` (`:1850`). Una entrada no autorizada por el path admin se "lava" a traves del allowlist del path system del segundo hive.

Contrato/documentacion:

- `docs/onworking COA/sy_router_tasks.md:216`: el chequeo de origen en SY.orchestrator es "ademas de policy/router" (defensa en profundidad en el nodo, no solo en el router).
- El guard `scripts/router_dispatcher_guards/origin_auth_gates_present.sh` exige `admin_origin_authorized`/`build_admin_forbidden_response` en SY.admin (gateway canonico de `ADMIN_COMMAND`). El orchestrator recibio el gate del lado SYSTEM pero no su equivalente del lado ADMIN.

Impacto:

Un nodo de la malla (o comprometido) que pueda direccionar `SY.orchestrator@<hive>` con un `ADMIN_COMMAND` mutante ejecuta la operacion sin pasar por allowlist. Distinto de F4: aqui el camino ADMIN ni siquiera invoca el allowlist. Mitigante: depende de cuan permisiva sea la policy OPA del router para `ADMIN_COMMAND`, y `add_hive`/`remove_hive` tienen el gate parcial `is_motherbee`.

Recomendacion:

Agregar en `handle_admin` un gate de origen para acciones admin protegidas (predicado de acciones + `admin_origin_authorized` + `build_admin_forbidden_response`), en paridad con SY.admin, antes del match de `:1416`. Tests: negativo (origen no permitido -> FORBIDDEN) y positivo (`SY.admin@<hive>`). Incluir al orchestrator en `origin_auth_gates_present.sh`.

### F1 - Alta - `remove_hive` usa SSH operativo contra el contrato v2

Verificacion: lectura + git blame.

Evidencia:

- `remove_hive_cleanup_via_ssh` existe (`src/bin/sy_orchestrator.rs:7397`); unicas guardas son address vacio (`:7399`) y key inexistente (`:7403`). Usa `ssh_with_key(address, key, cleanup_cmd, BOOTSTRAP_SSH_USER)`.
- Camino socket OK (`:7476-7499`): aunque `socket_cleanup_ok=true`, si `address` no esta vacio se llama igual `remove_hive_cleanup_via_ssh(&address)` (`:7479`) como "verificacion" -> `remote_cleanup=socket_ok_ssh_verified`, `remote_cleanup_via=socket+ssh` (`:7494-7495`).
- Camino socket fallo/timeout (`:7500-7524`): fallback SSH -> `socket_timeout_ssh_ok`/`socket_failed_ssh_ok`, `remote_cleanup_via=ssh` (`:7518-7524`).
- No hay early-return ni flag de config que apague el SSH. `address` se relee de `info.yaml` (`:7441-7449`), asi que para un hive normal con address valido y key presente (sembrada por `scripts/install.sh`), SSH se ejecuta en cada `remove_hive`.

Contrato/documentacion:

- `docs/onworking COA/sy_orchestrator_v2_tasks.md:10-12`: SSH solo para bootstrap de `add_hive`; "sin fallback SSH operativo".
- `:21`: `[x] remove_hive sin fallback SSH: online por socket o cleanup local` (marcado cerrado).
- `:103-105`: E2E-3 offline -> `remote_cleanup in {socket_timeout,local_only}` y `remote_cleanup_via=local_only`.
- El codigo **rompe el propio gate E2E-3**: `scripts/orchestrator_remove_hive_socket_e2e.sh:163-164` exige `socket_ok` exacto en online (recibe `socket_ok_ssh_verified`), y `:202-205` exige `local_only` en offline (puede recibir `ssh`). El script fuerza offline parando solo el orchestrator remoto (`:137`), sin inutilizar SSH, asi que el fallback prospera.

Matiz ("reintroduce"): el git blame muestra que el commit `cd4967c` agrego la "verificacion" SSH del camino socket-OK (eso es reintroduccion genuina); el fallback SSH del camino offline (`:7500-7524`) ya preexistia. La conclusion (SSH operativo presente contra contrato) es correcta en ambos casos.

Impacto:

`remove_hive` muta el worker por SSH aunque la operacion v2 deberia ser socket-only o local-only, y rompe el contrato y su test de cierre. Mitigante: el resultado es idempotente (el cleanup ya ocurrio por socket; el directorio local se borra igual) y degrada con gracia si la key/SSH fallan.

Recomendacion:

Eliminar `remove_hive_cleanup_via_ssh` del flujo. Para online, reportar solo `socket_ok`/`socket`. Para timeout/fallo de socket, continuar con cleanup local y reportar `socket_timeout`/`local_only` con `remote_cleanup_via=local_only`. Tras eso los gates E2E-3 online y offline pasan sin tocar el script.

### F2 - Alta - `add_hive` socket-only puede caer a bootstrap SSH

Verificacion: lectura.

Evidencia:

- Fast path detecta orchestrator remoto en LSA via `wait_for_remote_orchestrator_node` -> `socket_only_ready=true` (`src/bin/sy_orchestrator.rs:14821-14822`).
- Tres caminos llegan a SSH bootstrap (`:14991`) aun con `socket_only_ready=true`:
  1. Fallo de probe (`:14824`): el `else` se salta, solo hay `tracing::warn!`, y el control sale hasta `:14991`. **No pasa por `is_socket_only_unreachable_error`** — cualquier fallo de `GET_VERSIONS` (incluido payload non-ok) cae a SSH. Es la violacion mas cruda.
  2. Finalize unreachable/timeout (`:14847-14856`): `is_socket_only_unreachable_error` (`:14586`, usada en `:14849`) matchea timeout/unreachable -> `finalize = None` -> el bloque finalize se salta -> SSH.
  3. Finalize con error NO-unreachable (`:14857-14876`): retorna `FINALIZE_FAILED`. Unico branch que cumple el contrato.

Contrato/documentacion:

- `docs/onworking COA/sy_orchestrator_v2_tasks.md:10-12`: si el worker ya esta online por socket, no se usa SSH.
- `:22`: `[x] add_hive socket-only no cae a bootstrap SSH si falla finalize (FINALIZE_FAILED)` — sin excepcion por timeout/unreachable.
- `:101-102`: `[x] E2E-2` espera `bootstrap_mode=socket_only_existing_orchestrator`, cero pasos SSH. Ningun script referencia ese modo (grep vacio): el item esta marcado cerrado pero **ningun test lo guarda**.

Impacto:

Un worker ya operativo puede ser reprovisionado por SSH (siembra de key/sudoers + `sync_core_to_worker:15120` que copia binarios core) por una falla transitoria de probe/finalize. Contradice el contrato y reescribe configuracion de un host que ya estaba arriba.

Recomendacion:

Una vez `socket_only_ready=true`, bloquear todo fallback SSH en ambos puntos (probe y finalize-unreachable): devolver error explicito (`FINALIZE_FAILED` o codigo de probe), sin continuar a bootstrap. Agregar E2E-2 que asierte cero pasos SSH.

### F3 - Alta - `add_hive` borra directorios antes de validar `hive_id`

Verificacion: **empirica** (rustc confirmo que `Path::join` no normaliza `..` y `remove_dir_all` escapa el root).

Severidad bajada de Critica a Alta respecto de la version anterior (ver abajo).

Evidencia:

- Worker `add_hive_flow`: `hive_dir = root.join(hive_id)` (`:14743`, root = `hives_root()` en `:14742`); `hive_exists` (`:14744`); `hive_partial_exists` (`:14760`); `fs::remove_dir_all(&hive_dir)` (`:14765`); y recien `valid_hive_id` (`:14773`).
- Egress `add_egress_hive_flow`: mismo orden — `:15677`, `:15681`, `:15682`, validacion en `:15686`.
- `valid_hive_id` (`:16064`) rechaza vacio, `len>64`, y todo byte fuera de `[A-Za-z0-9_-]` (asi `/`, `.`, `..` quedan rechazados — pero tarde).
- El dispatcher toma `hive_id` crudo de `msg.payload` sin sanitizar (`:1580-1584`); unica barrera previa es `is_motherbee` (`:1571`).
- Reproducido: `root.join("../victim")` = `.../hives/../victim` (preserva `ParentDir`); `fs::remove_dir_all` sobre esa ruta borro recursivamente un directorio hermano fuera del arbol.

Preconditions (que la version anterior omitia): el target debe (a) existir ya como directorio y (b) carecer de `info.yaml` (si lo tiene, `hive_exists` cortocircuita con `HIVE_EXISTS` antes del borrado). Esto acota pero no elimina el impacto.

Impacto:

Un `add_hive(hive_id='../<dir-existente-sin-info.yaml>')` borra ese directorio fuera de `storage/hives`. Es destructivo. Mitigante (por eso Alta y no Critica): doble gate — `is_motherbee` + `ADMIN_COMMAND` autenticado. Es path traversal de operador privilegiado, no destruccion remota no autenticada. Contraste: `remove_hive_flow` valida primero (`valid_hive_id:7415`) y recien luego hace `root.join` — patron correcto del cual `add_hive`/`add_egress` se desvian.

Recomendacion:

Mover `valid_hive_id` (y `valid_address`) al inicio de ambos flujos, antes de cualquier `join`/`hive_exists`/`hive_partial_exists`/`remove_dir_all`, o sanitizar `hive_id` en el dispatcher (`:1580`). Adicionalmente, antes de borrar, canonicalizar y verificar que la ruta siga bajo `hives_root()`.

### F4 - Alta - La allowlist de origen `system` no es realmente configurable

Verificacion: lectura + test.

Evidencia:

- `load_system_allowed_origins` (`src/bin/sy_orchestrator.rs:2791-2805`) lee `ORCH_SYSTEM_ALLOWED_ORIGINS` (default `SY.admin,WF.orch.diag`) y expande entradas sin `@` a `@<hive_id>`.
- `is_allowed_system_source_name` (`:1845-1854`): tras rechazar None/vacio, retorna true si `system_allowed_origins.contains(name)` **OR** `name.starts_with` de `SY.orchestrator@`, `SY.admin@`, `SY.wf-rules@` o `WF.orch.diag@`. Los 4 prefijos son incondicionales: no se desactivan por env var.
- Test `system_source_with_allowed_src_l2_name_passes_auth` (`:18391`): inserta solo `SY.admin@motherbee` pero asierta que `WF.orch.diag@motherbee` pasa (`:18401-18404`) sin haberlo insertado — fija el bypass hardcodeado. El fix debe tocar este test.

Contrato/documentacion:

- `docs/onworking COA/sy_router_tasks.md:216,218`: hardening por allowlist explicita, configurable via `ORCH_SYSTEM_ALLOWED_ORIGINS`.
- `:220`: recomendacion productiva `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin`, dejando `WF.orch.diag` solo para pruebas controladas. **Esa recomendacion es inalcanzable**: `WF.orch.diag@` esta hardcodeado en `:1853`, asi que el tooling de diagnostico mantiene acceso a `SPAWN_NODE`/`KILL_NODE`/`SYSTEM_UPDATE` en produccion aunque el operador siga la guia.
- `SY.orchestrator@` y `SY.wf-rules@` ni siquiera figuran en el contrato como origenes de sistema (el contrato solo menciona `SY.admin` y `WF.orch.diag`): 2 origenes hardcodeados ausentes de la spec.

Impacto:

Un operador no puede sacar `WF.orch.diag@*` ni `SY.wf-rules@*` de los origenes aceptados. Cualquier nodo con uno de esos nombres L2 queda autorizado para acciones protegidas si logra rutear el mensaje. Mitigante: el chequeo es "ademas de policy/router" (`:216,291`), hay defensa en profundidad upstream.

Recomendacion:

Mover los 4 prefijos a defaults del allowlist configurable (que `load_system_allowed_origins` los siembre y `is_allowed_system_source_name` solo consulte el set), de modo que `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin` realmente excluya `WF.orch.diag`. Si se quiere wildcard, que sea sintaxis explicita de config, no una excepcion silenciosa. Actualizar el test `:18391`.

### F9 - Alta - `add_hive` no es idempotente: timeout deja el hive `pending` atascado

Verificacion: lectura.

Evidencia:

- En bootstrap, tras los gates de conectividad, `add_hive_flow` escribe `info.yaml` **antes** de evaluar el resultado: `status = if wan_connected && orchestrator_connected { "connected" } else { "pending" }` (`:15426`) y `write_hive_info` incondicional (`:15428`). Recien despues retorna error: `WAN_TIMEOUT` (`:15436`), `WORKER_ORCHESTRATOR_TIMEOUT` (`:15453`).
- `hive_exists` (`:16054`) solo comprueba que exista `info.yaml`, ignora `status`. `hive_partial_exists` (`:16059`) considera "parcial" solo cuando `info.yaml` **no** existe. Como el estado `pending` tiene `info.yaml`, un reintento entra por `hive_exists==true` y retorna `HIVE_EXISTS` inmediatamente, sin reintentar bootstrap ni finalize.
- El finalize que el `pending` nunca ejecuta: `add_hive_finalize_via_socket` (`:15477`), `apply_add_hive_ssh_controls_after_finalize` (`:15582`), `status:connected` (`:15622`).
- Ningun watchdog/reconciler lee `status=="pending"` para re-drivear finalize (grep: los unicos consumidores de `status` son listado/inventario).
- Espejo en `add_egress_hive_flow`: escribe `pending` (`:15946-15953`), retorna `WAN_TIMEOUT`/`ORCHESTRATOR_TIMEOUT` (`:15960/15971`), aplica SSH controls recien en `:15989`.

Impacto:

El worker arranca pero su WAN/orchestrator tarda mas de 60s en aparecer en LSA (red lenta, primer arranque, syncthing inicial). `add_hive` escribe `info.yaml=pending` y devuelve timeout. El worker queda operativo; el operador reintenta para completar el finalize (peer-link syncthing, `info=connected`, `restrict_ssh`) y recibe `HIVE_EXISTS`: el hive queda **sin syncthing peer-link ni controles SSH** permanentemente, sin via de recuperacion salvo `remove_hive` manual + re-add. Relevante de seguridad en el caso egress: escribe `pending` antes de aplicar los SSH controls.

Recomendacion:

Hacer que el guard de entrada distinga `status=="pending"`/`"connected"` (o tratar `pending` como parcial), permitiendo que el reintento caiga en el fast-path socket-only idempotente y complete el finalize.

### F10 - Alta - Fallo de hardening SSH en egress es no-fatal y reporta `status:ok`

Verificacion: lectura.

Evidencia:

- `add_egress_hive_flow` (`src/bin/sy_orchestrator.rs:15989-16001`): el `Err` arm de `apply_add_hive_ssh_controls_after_finalize` solo hace `tracing::warn!("egress ssh hardening failed; continuing")` y setea `(restrict_ssh_applied=false, restrict_ssh_mode="error")`, cayendo al `status:ok` de `:16018-16035`.
- El flujo worker trata el **mismo helper** como fatal (`:15582-15613`): retorna `SSH_HARDEN_FAILED`/`SSH_KEY_FAILED`.
- El helper retorna `Err` tanto en fallo de restriccion `from-only` (`:16380-16385`) como en fallo de `disable_remote_password_auth`/verificacion (`:16394-16399`). `disable_remote_password_auth_with_access` reinicia sshd remoto (`:16457-16463`) — punto de fallo transitorio real.
- `harden_ssh=true` implica `restrict_ssh=true` por default (`resolve_add_hive_restrict_ssh:16151-16164`).
- El payload egress emite `harden_ssh: harden_ssh` (`:16028`, eco del request) y no tiene campo de estado dedicado para la mitad harden.

Impacto:

El host egress — el boundary expuesto a internet — puede quedar con `PasswordAuthentication` habilitado y/o bootstrap key sin `from=` mientras el operador ve `ok`. El flujo worker habria marcado la condicion identica como fallo duro. Mitigante: la mitad `restrict` si deja senal observable (`restrict_ssh_mode="error"`, `restrict_ssh=false`); una automatizacion atenta puede inferir el fallo.

Recomendacion:

Alinear `add_egress_hive_flow` con `add_hive_flow`: retornar `SSH_HARDEN_FAILED`/`SSH_KEY_FAILED` y no devolver `status:ok` cuando el helper falla en un boundary egress. Como minimo, agregar `harden_ssh_applied` real.

### F11 - Alta - `remove_hive` de un egress no des-aprovisiona el estado NAT

Verificacion: lectura.

Evidencia:

- El script de cleanup remoto (`src/bin/sy_orchestrator.rs:7366-7375`) solo hace stop/disable/kill de servicios fluxbee + `rm -rf /var/lib/fluxbee/nodes/*` y `/var/lib/fluxbee/state/nodes/*`. No borra ficheros de egress.
- `remove_hive_flow` (`:7414-7603`) lee `address` y `syncthing_device_id` de `info.yaml` pero nunca lee `role` ni invoca teardown (grep de role/egress/nft/sysctl/reconcile en ese rango = 0 hits).
- `delete_egress_nft_table_if_present` (`:3962`) tiene un unico callsite: dentro de `reconcile_egress_nat` (`:4037`); nunca en remove.
- Ficheros persistentes que escribe `reconcile_egress_nat` con IPv4 forwarding ON + MASQUERADE: `/etc/sysctl.d/99-fluxbee-egress.conf` (`:3703`), `/etc/sysctl.d/99-fluxbee-conntrack.conf` (`:3704`), `/etc/modprobe.d/fluxbee-conntrack.conf` (`:3705`), `/etc/nftables.d/fluxbee-egress.nft` (`:3706`).

Contrato/documentacion:

- `docs/edge-egress-nat-spec.md` (L349/L371): los ficheros son "persistent across reboots by reapplication" y viven bajo `/etc/sysctl.d` y `/etc/nftables.d`.
- El contrato de remove (`docs/onworking COA/edge_egress_nat_tasks.md`) no incluye teardown de egress; el E2E solo valida `remote_cleanup`, nunca estado egress.

Impacto:

Tras `remove_hive` de un egress, el host removido sobrevive al reboot con IPv4 forwarding habilitado y MASQUERADE LAN->WAN activos, sin orquestador que lo gobierne: router/NAT huerfano y persistente fuera de gobierno. Mitigante: el box ya salio de la flota y el re-armado solo ocurre al reboot.

Recomendacion:

En `remove_hive_flow`, leer `role` de `info.yaml` y, si es egress, extender el cleanup (script + via socket) para `nft delete table inet fluxbee_egress` (reusar `delete_egress_nft_table_if_present`) + `rm` de los 4 ficheros + `sysctl --system`. Alternativa: un `EGRESS_TEARDOWN` en el handler.

## Findings - Media

### F5 - Media - Egress reporta `ok` con verificacion incompleta

Verificacion: lectura. **Reescrito**: la version anterior fundaba el finding en `edge_ip`, premisa que resulto incorrecta. El problema real es otro.

Correccion de la premisa: el spec §8.2 (`docs/edge-egress-nat-spec.md:411-435`) nunca pide asignar `edge_ip` a `lan_iface`; `edge_ip` es config de host preexistente que el orchestrator usa para validar (`:3814-3819`), reportar y como gateway de la ruta de workers. Que `egress_nft_ruleset` (`:3891`) no use `edge_ip` es spec-compliant, no un defecto. Ese eje de la version anterior se descarta.

Lo que si es problema:

- `add_egress_hive_flow` responde `egress_nat_applied: true` (`:16027`) y **omite** los 4 campos del spec §9: `egress_ipv4_forwarding`, `egress_ipv6_blocked`, `egress_conntrack_tuned`, `egress_internet_reachable` (no existen como claves JSON en ningun lado; grep vacio). El payload remite a "ver journal del host egress".
- La inferencia "orchestrator activo => NAT aplicado" es valida para `nat_applied`/`ipv4_forwarding`/`ipv6_blocked` porque `reconcile_egress` es fatal en bootstrap (`bootstrap_local:790-797`, CR-1). **Pero** `internet_reachable` (`reconcile_egress_nat:4046`) solo `warn`ea si es false (`:4047-4052`), no retorna `Err`. Un egress con WAN uplink caido queda `active`, registra en LSA y motherbee devuelve `status:ok egress_nat_applied:true`.
- `check_internet_reachable` (`:3979`) prueba el uplink real, distinto de `wait_for_wan` (`:16909`) que solo verifica frescura del hive en la SHM LSA (plano de control overlay). Los gates de `add_egress_hive_flow` no consultan el uplink.

Contrato/documentacion:

- `docs/edge-egress-nat-spec.md:471-488` (§9): exige los 4 campos y "No silent success". §3.5 (`:70-72`): "Fail loud, never silent".
- `docs/onworking COA/edge_egress_nat_tasks.md:105` (T-VER-1): en v1 esos detalles quedan en el journal del egress y no viajan a motherbee. El tasks doc prima sobre el spec (`:25`), asi que la **no-transmision** de los campos es carve-out v1 documentado, no contradiccion.

Impacto:

Un egress cuyo `wan_iface` resuelve pero no tiene ruta a internet (mal cableado) reporta `ok` mientras la funcion central del nodo (NATear a internet) no funciona; la unica evidencia es un `warn` en el journal remoto. La omision de los campos del payload es por diseno v1; el problema real es que `internet_reachable=false` no degrada el status.

Recomendacion:

Decidir si `internet_reachable=false` debe degradar el status a `warn`/`pending` (coherente con §3.5) en vez de `ok`. A futuro (T-VER-1/T-VER-3), que `add_hive role=egress` recoja del host remoto los 4 campos reales y los incluya en el payload. Ver F16 para `conntrack_tuned`.

### F6 - Media - El watchdog egress no corrige drift parcial de reglas/sysctl

Verificacion: lectura.

Evidencia:

- `watchdog_egress_reconcile` rama Egress (`src/bin/sy_orchestrator.rs:1291-1297`): re-aplica NAT solo dentro de `if !nft_table_loaded()`.
- `nft_table_loaded` (`:3939-3946`) solo corre `nft list table inet fluxbee_egress` y mira exit code: verifica **presencia** de la tabla, no contenido. Una tabla presente con reglas flusheadas/editadas pasa como sana -> no re-aplica.
- `reconcile_egress_nat` es el unico lugar que aplica sysctl (`:4017`), conntrack (`:4030`) y reescribe reglas (`:4036-4040`); el watchdog solo lo invoca tras `!nft_table_loaded()`, asi que drift de sysctl/conntrack no se corrige mientras la tabla exista.
- Rama Worker (`:1299-1323`): solo `default_route_via` + `ip route replace`. El bloque IPv6 del worker (`worker_ipv6_sysctl_content` + `apply_sysctl_system`) vive en `reconcile_worker_egress` (`:4095-4098`), que solo corre en bootstrap, nunca en el tick. (Esto absorbe lo que en el analisis figuraba como finding separado del IPv6 del worker.)
- El watchdog es silencioso ante drift parcial: si alguien flushea una chain, no hay `warn` ni re-apply hasta el proximo reboot.

Contrato/documentacion:

- `docs/edge-egress-nat-spec.md:343` (§6.5): "On startup and on reconcile ... apply the network configuration described in §8" (§8 = sysctl + conntrack + nftables) — tension real con el gap.
- `docs/onworking COA/edge_egress_nat_tasks.md:145` (CR-5): describe **exactamente** este comportamiento y lo declara intencional ("egress re-aplica NAT solo si la tabla nft desaparecio"). Es decision de diseno documentada, no descuido. (La cita `:143`/CR-1 de la version anterior es imprecisa: trata de bootstrap-fatal, no del watchdog.)

Impacto:

La reconciliacion detecta desaparicion total de la tabla, pero no drift parcial de reglas/sysctl/conntrack. Cambios manuales pueden quedar activos hasta el proximo reboot. Mitigante: nodo egress es infra dedicada; el reboot re-corre el reconcile completo.

Recomendacion:

En el tick egress, ademas de `nft_table_loaded()`, comparar contenido de la tabla con el ruleset esperado (o re-aplicar idempotente `write_file_if_changed` + sysctl + conntrack cada N ticks). En worker, re-aplicar el sysctl IPv6 idempotente. `reconcile_egress_nat` ya es idempotente, asi que correrlo periodicamente no rompe nada salvo la ventana de ms del delete+apply (ver F25).

### F12 - Media - Inyeccion de reglas nft via `wan_iface`/`lan_iface` sin validar

Verificacion: **empirica** (PoC con serde_yaml 0.9 + simulacion del ruleset).

Causa raiz compartida con F7 y F13: ausencia de allowlist de nombre de interfaz.

Evidencia:

- `egress_nft_ruleset` (`src/bin/sy_orchestrator.rs:3905,3910`) interpola crudo `iifname "{lan_iface}"` / `oifname "{wan_iface}"` via `format!` (`:3915-3916`).
- `reconcile_egress_nat` (`:4035-4040`) escribe el ruleset a `EGRESS_NFT_PATH` y lo ejecuta con `nft -f`.
- En el host egress remoto, `add_egress_hive_flow` (`:15842-15852`) escribe `wan_iface`/`lan_iface` verbatim dentro del scalar YAML; el orquestador remoto carga `hive.yaml` con `serde_yaml::from_str` hacia `EgressSection` (sin validacion post-deserializacion) y lo pasa a `reconcile_egress_nat`.
- PoC: un newline **literal** solo corrompe el YAML (DoS de provisioning), pero el laundering por escapes YAML (`eth0\"\n ip saddr 0.0.0.0/0 accept #`) produce un YAML valido que serde_yaml re-decodifica a comilla + newline reales -> rompe el quoting del nft y deja una regla inyectada en la chain forward que derrota el `policy drop`.

Impacto:

Inyeccion de reglas de firewall ejecutadas como root via `nft -f` en el host egress, anulando la garantia de `forward policy drop` (spec §8.2). Mismo modelo de amenaza que F7 (motherbee/admin), no atacante remoto no autenticado.

Recomendacion:

El mismo fix de F7 (allowlist `^[A-Za-z0-9._-]{1,15}$` en `resolve_egress_nat_config`) cierra este vector.

### F13 - Media - Corrupcion/inyeccion YAML en el `hive.yaml` remoto

Verificacion: **empirica** (simulacion YAML). Causa raiz compartida con F7/F12.

Evidencia:

- Plantilla `add_egress_hive_flow:15842`: `wan_iface: "{wan_iface}"` / `lan_iface: "{lan_iface}"` con interpolacion directa, sin escapado. `write_remote_file` usa heredoc `<<'EOF'` (sin expansion shell) pero preserva el `"` y los `\n` literales en `/etc/fluxbee/hive.yaml`.
- El remoto parsea con `serde_yaml::from_str` -> `HiveFile` (`load_hive:3036-3039`) sin `deny_unknown_fields`.
- Simulacion: `wan_iface = eth0"\n  enabled: false\n  injected_key: "pwned` produce `egress.enabled=false` + clave inyectada parseable; un dedent produce `ParserError` (hive.yaml corrupto -> falla de arranque remoto).

Correccion respecto del analisis crudo: `storage_path` **no** es atacante-controlable (viene de `state.storage_path:15832`, config del operador, no del payload). Solo `wan_iface`/`lan_iface` son el input no confiable.

Impacto:

Un `add_hive role=egress` con `"`+newline en la interfaz corrompe el `hive.yaml` remoto (DoS de arranque del orchestrator remoto) o inyecta/sobrescribe claves del mapping egress. Es defensa-en-profundidad / validacion de input con DoS concreto del nodo egress.

Recomendacion:

Mismo fix de F7. Un solo allowlist de interfaz cierra F7 + F12 + F13.

### F14 - Media - `watchdog_tick` no es panic-safe

Verificacion: lectura.

Evidencia:

- `src/bin/sy_orchestrator.rs:715-728`: `compare_exchange(false,true)` sobre `watchdog_running` como gate; `tokio::spawn` detachado sin `catch_unwind`/`AssertUnwindSafe` ni Drop guard; el reset `watchdog_flag.store(false)` (`:727`) corre solo **despues** de que `watchdog_tick(&...).await` (`:726`) retorna. Los unicos escritores son `:720` (true) y `:727` (false); no hay recovery. El rechazo del gate solo loguea `tracing::debug!` (`:730`), por debajo del nivel default `info` (`:551`).
- Vector de panic: `node_name` (`:7204-7205`) hace `&entry.name[..len]` con `name:[u8;256]` y `name_len:u16` hasta 65535 (`shm/mod.rs:150-151`); `read_router_snapshot` copia `NodeEntry` verbatim sin clamp (`shm/mod.rs:2827-2828`). Si `name_len>256` -> slice out-of-bounds -> panic, la tarea muere, `:727` nunca corre. Ver F20.

Impacto:

Un panic dentro de la tarea spawneada salta el `store(false)` y el flag queda en `true` para siempre; cada `compare_exchange` posterior falla -> se pierde restart de servicios, verify/retain de runtime, blob GC y reconcile egress, en silencio (solo `debug`). El self-healing se detiene mientras el orchestrator sigue reportandose sano. Mitigante: el writer normal clampa `name_len` a 256, asi que el disparo via `node_name` exige SHM corrupto/torn read; pero `watchdog_tick` hace mas trabajo propenso a panic.

Recomendacion:

Mover el reset a un Drop guard (o `catch_unwind`/`AssertUnwindSafe` alrededor del cuerpo del tick), de modo que un panic no deshabilite el watchdog para siempre. Aplicar tambien el clamp de F20.

### F15 - Media - La allowlist acepta sufijo de hive arbitrario (bypass cross-hive)

Verificacion: lectura. Extiende F4 con el angulo cross-hive.

Evidencia:

- `is_allowed_system_source_name` (`src/bin/sy_orchestrator.rs:1849-1853`): el unico match exacto (`system_allowed_origins.contains(name)`) se confina al hive local (`load_system_allowed_origins:2797-2802` expande bare a `name@<hive_id>`), pero los 4 `starts_with(...@)` aceptan cualquier sufijo de hive.
- `src_l2_name` es autoritativo (stampeado por el router, no spoofable por el sender; test `router_overwrites_spoofed_src_l2_name`). Pero `normalize_name` (`src/router/mod.rs:3712-3718`) conserva verbatim un nombre que ya trae `@<hive>`, sin validar que el sufijo sea el hive local ni proteger nombres reservados; un nodo puede registrarse como `SY.admin@<hive-ajeno>`.
- `vpn_allows_between` (`src/router/mod.rs:4974-4976`) deja pasar SYSTEM hacia un system node (el orchestrator) cross-hive.

Impacto:

Un `SY.admin`/`WF.orch.diag` legitimo de otro hive, o un nodo que registre uno de esos prefijos con `@<sufijo arbitrario>`, pasa `is_allowed_system_source_name` y puede ejecutar `SPAWN_NODE`/`KILL_NODE`/`NODE_CONFIG_SET` en el orchestrator victima, anulando el confinamiento same-hive que el operador fijo via `ORCH_SYSTEM_ALLOWED_ORIGINS`.

Recomendacion:

El allowlist debe ser match exacto de nombre completo `name@hive` sin los `starts_with` por prefijo; si se quiere permitir cualquier instancia local de `SY.admin`/etc., comparar contra `base@<hive_local>`. Complementariamente, `normalize_name`/registro deberia rechazar que un nodo se registre con prefijo reservado `SY.*`/`WF.orch.diag*` o con un `@<hive>` distinto al local. Tests existentes (`:18391`, `:18408`) fijan el comportamiento por prefijo y habria que actualizarlos.

### F16 - Media - `egress_conntrack_tuned` se reporta `true` incondicionalmente

Verificacion: lectura.

Evidencia:

- `apply_conntrack_live()` (`src/bin/sy_orchestrator.rs:3929-3937`) retorna `()` y traga errores con `tracing::warn!` ("conntrack max not applied live"); el doc-comment (`:3927-3928`) reconoce que las keys solo existen una vez cargado el modulo `nf_conntrack`.
- `reconcile_egress_nat` (`:4030-4031`): `apply_conntrack_live(); verification.conntrack_tuned = true;` incondicional — no hay nada que checkear porque la funcion devuelve `()`.
- El hashsize se escribe solo a `/etc/modprobe.d/fluxbee-conntrack.conf` (`:4026-4029`) = reboot-only. No hay lectura de `nf_conntrack_count`/`nf_conntrack_max` para verificar.

Contrato/documentacion:

- `docs/onworking COA/edge_egress_nat_tasks.md` (T-NET-5): exige reportar `egress_conntrack_tuned` distinguiendo "live" de "pending-reboot". El codigo implemento las escrituras pero no la distincion.
- `docs/edge-egress-nat-spec.md:443` (§8.4): "the connection tracking table fills and packets are dropped silently".

Impacto:

En el camino comun (modulo `nf_conntrack` aun no cargado al momento del reconcile -> `sysctl -w` falla -> `warn`), el campo igual queda `true`, sobre-afirmando el estado. Bajo carga sostenida pre-reboot es el escenario de silent-drop que §8.4 advierte. Es mas un misreport que un outage garantizado (el archivo persistido toma efecto en el next boot).

Recomendacion:

Convertir `conntrack_tuned` a un enum/string (`"live"`|`"pending-reboot"`) o agregar `conntrack_live_applied` verificando lectura de `nf_conntrack_max` tras el `sysctl -w`, en vez del bool incondicional.

### F17 - Media - La tabla nft egress no se carga al boot

Verificacion: lectura.

Evidencia:

- El sysctl `ip_forward=1` es persistente e independiente del orchestrator: vive en `/etc/sysctl.d/99-fluxbee-egress.conf` (`:3703`, contenido en `egress_sysctl_content:3848`) y lo aplica `systemd-sysctl` temprano en cada boot.
- La tabla nft no: `EGRESS_NFT_PATH = /etc/nftables.d/fluxbee-egress.nft` (`:3706`); `reconcile_egress_nat` la escribe y aplica con `nft -f` (`:4036-4040`), pero nada en el repo habilita `nftables.service`, agrega un include, ni un unit que cargue ese fichero al boot (grep en `src/` `scripts/` `*.service` `*.sh` = 0 referencias salvo la const y su uso runtime).
- Timing: `reconcile_egress` (para egress) corre en `bootstrap_local:790`, **despues** de `systemd_start("rt-gateway")` (`:782`) + `wait_for_router_ready` hasta 30s (`:783`) + `wait_for_nats_ready` hasta 20s (`:784`). El unit `sy-orchestrator` es `After=network.target rt-gateway.service` (`:15883`). Ventana = desde `systemd-sysctl` (boot temprano) hasta `:790` = segundos a ~30-50s, y se reabre en cada `Restart=always`.

Impacto:

Entre que `systemd-sysctl` aplica `ip_forward=1` y `sy-orchestrator` corre `reconcile_egress`, el host egress tiene IPv4 forwarding habilitado **sin** tabla `fluxbee_egress`: sin masquerade (paquetes con saddr privado salen al WAN, egress roto), sin `forward policy drop` y sin drop de forwarding IPv6. El boundary controlado esta ausente toda la ventana de arranque tras cada reboot. Mitigante: ventana transitoria, auto-cierra; los paquetes con saddr privado normalmente los descarta el upstream.

Recomendacion:

Persistir la carga de la tabla al boot (`enable nftables.service` + include de `/etc/nftables.d/fluxbee-egress.nft`, o un `ExecStartPre`), o invertir el orden para que `ip_forward` solo quede ON despues de aplicar la tabla — atando el forwarding al ciclo del orchestrator en vez de persistirlo independientemente.

### F18 - Media - TOCTOU: `add_hive`/`remove_hive` no toman ningun lock

Verificacion: lectura.

Evidencia:

- El dispatch de `add_hive` (`src/bin/sy_orchestrator.rs:1571`) y `remove_hive` (`:1546`) no toma ningun lock. Existe `runtime_lifecycle_lock` (`:481`) para updates, pero no se usa aca.
- `add_hive_flow` ejecuta `hive_partial_exists` -> `remove_dir_all` -> `create_dir_all` (`:14760-14817`) sin serializacion.

Impacto:

Un `remove_hive` concurrente sobre el mismo `hive_id`, o dos `add_hive` en paralelo, pueden corromper `storage/hives` (carrera entre el check de existencia, el borrado y la creacion). Combinado con F3 (borrado antes de validar), la ventana es explotable por timing. Mitigante: requiere comandos admin concurrentes sobre el mismo `hive_id`.

Recomendacion:

Serializar por `hive_id` (lock por-hive) o reusar `runtime_lifecycle_lock` para todo el lifecycle de `add_hive`/`remove_hive`.

## Findings - Baja

### F19 - Baja - `list_hives` etiqueta un host egress como `role=worker` y omite `role` en workers

Verificacion: lectura.

Evidencia:

- La entrada local sintetizada (`src/bin/sy_orchestrator.rs:7340-7351`) ramifica solo en `is_motherbee`: `let local_role = if state.is_motherbee { "motherbee" } else { "worker" };`. `OrchestratorState` tiene `role: HiveRole` (`:610`) con `as_str()` que ya devuelve `"egress"` (`:199-205`), pero no se usa. En un host egress `is_motherbee==false` -> se auto-reporta `worker`.
- Esta rama se alcanza en hosts no-motherbee porque `write_hive_info` solo se invoca para el hive *target* (remoto), nunca para el hive local.
- Los payloads `info.yaml` de workers omiten `role` (`:14946-14951`), mientras el egress incluye `role:"egress"` (`:15946-15952`); `list_hives` eco-devuelve sin normalizar -> una respuesta `GET /hives` mezcla workers sin `role` con egress con `role`.

Impacto:

Inconsistencia de inventario: un consumidor (admin UI, filtros por role) ve un egress como `worker` y workers sin `role`. Solo metadata de display; ningun control de flujo/seguridad lee este campo (el orchestrator usa `state.role`/`state.is_motherbee` internamente).

Recomendacion:

Usar `state.role.as_str()` para la entrada local, y agregar `"role":"worker"` a los payloads `info.yaml` de worker (o defaultear `role` ausente a `"worker"` en `read_hive_info`).

### F20 - Baja - Slices de nombre SHM sin clamp (`node_name` y hermanos)

Verificacion: lectura.

Evidencia:

- `src/bin/sy_orchestrator.rs:7204-7205`: `let len = entry.name_len as usize; let name_bytes = &entry.name[..len];` sin clamp. Hermanos identicos en `:5585-5586`, `:5690-5691`, `:10449-10450`, `:11061`. `name:[u8;256]`, `name_len:u16` (`shm/mod.rs:150-151`), asi que `name_len` puede valer hasta 65535 a nivel de tipo.
- La ruta de lectura no clampa (`read_router_snapshot:2828`, `read_lsa_snapshot:2958` copian la entrada cruda). El patron correcto ya existe en 4 sitios (p.ej. `read_string:2855` usa `len.min(buf.len())`, `bytes_to_string` del router `:4944`).

Impacto:

Bug latente de robustez, **no reproducible en operacion correcta**: el writer legitimo (router) clampa `name_len<=256` via `copy_bytes_with_len` (`shm/mod.rs:2288-2293`) y el seqlock cubre lecturas desgarradas. El disparo exige un escritor SHM buggy/comprometido/desincronizado. Es el gatillo de panic de F14.

Recomendacion:

Cambiar a `&entry.name[..(entry.name_len as usize).min(entry.name.len())]` o saltar entradas con `name_len>256`, idealmente centralizado en un helper reusando `bytes_to_string`/`read_string`.

### F21 - Baja - Host egress sin filtrado inbound (input chain `policy accept`)

Verificacion: lectura. **Trade-off documentado de v1** (no es un defecto sorpresa).

Evidencia:

- `egress_nft_ruleset` (`src/bin/sy_orchestrator.rs:3896-3901`): `chain input { type filter hook input priority 0; policy accept; }` sin reglas allow.
- `ensure_core_firewall_local` (`:3660-3666`): early-return en `role==Egress` ("skipping ufw"), no abre ningun puerto; worker/motherbee si arman reglas ufw (`:3667-3691`).

Contrato/documentacion:

- `docs/edge-egress-nat-spec.md:64` (§3.3) y `:465` (§8.5) describen filtrado inbound (solo WAN + SSH de control desde motherbee) que la implementacion no cumple.
- `docs/onworking COA/edge_egress_nat_tasks.md:78` (T-NET-4): el filtrado inbound real se difiere a §11 para no arriesgar lockout en la box de testing; en v1 `policy accept`.

Impacto:

Divergencia spec-vs-codigo intencional y documentada. Exposicion latente acotada: el camino realmente expuesto (FORWARD/NAT hacia WAN) si esta filtrado (`policy drop` + solo ct established/related + LAN->WAN + ip6 drop, `:3902-3911`); el `input accept` es sobre un host en LAN interna.

Recomendacion:

Alinear §3.3/§8.5 con el estado v1 real (anotar que el filtrado inbound es non-v1) o agregar al input chain los allows minimos (WAN listen + SSH desde motherbee) con `policy drop` cuando se cierre el boundary.

### F22 - Baja - El flujo egress carece del fast-path socket-first del worker

Verificacion: lectura. **No destructivo** (refutado el framing de "deshace restricciones").

Evidencia:

- `add_hive_flow` worker tiene fast-path socket-first (`:14819-14988`); `add_egress_hive_flow` no, y corre el bootstrap SSH incondicional.
- El re-seed de key solo ocurre via canal password (`:15729-15745`) y la restriccion/hardening se aplica recien en `:15989`, despues de `write_hive_info` y de los gates que retornan temprano. En el caso parcial (interrumpido antes de `info.yaml`) no hay restriccion previa que deshacer. Los pasos son idempotentes (sudoers reescrito desde template, authorized_keys deduplica, `systemctl restart`).
- Ausencia documentada: `edge_egress_nat_tasks.md` T-ROLE-9 enumera los helpers que `add_egress_hive_flow` reusa, y los socket-first no estan en la lista.

Impacto:

Falta de paridad: re-provisionar un egress online reinicia `rt-gateway`/`sy-orchestrator` (restart, no wipe) en lugar de cortar via socket. El caso normal (con `info.yaml`) esta protegido por `HIVE_EXISTS`. Peor caso: restart de servicios en una ventana muy estrecha.

Recomendacion:

Agregar el fast-path socket-first al flujo egress, por paridad/idempotencia con el worker. Mejora, no bug de seguridad.

### F23 - Baja - `ipv4_forwarding`/`ipv6_blocked` se fijan `true` sin read-back del kernel

Verificacion: lectura. Informativa.

Evidencia:

- `reconcile_egress_nat:4017-4019`: `apply_sysctl_system()?; verification.ipv4_forwarding = true; verification.ipv6_blocked = true;`. `apply_sysctl_system` (`:3921-3925`) solo corre `sysctl --system` y mira exit code; no lee `/proc/sys`. Mismo patron en `reconcile_worker_egress:4096-4098`.
- Asimetria: `nat_applied` si se verifica con read-back real (`nft_table_loaded:4041-4044`).

Impacto:

`sysctl --system` exit 0 no garantiza el valor live (un fichero de mayor prioridad, key rechazada, `disable_ipv6` no honrado en ifaces ya up). Pero los flags no llegan al operador (solo al journal, no son superficie de contrato) y el control load-bearing del IPv6-leak en el host egress es la regla nft `meta nfproto ipv6 drop` (`:3906`), que si se verifica. Gap de precision de logging, no de seguridad.

Recomendacion:

Hacer read-back de `net.ipv4.ip_forward` y `disable_ipv6` antes de fijar los flags, por consistencia con `nat_applied`. Opcional.

### F25 - Baja - El re-apply de nft hace delete-then-apply no atomico

Verificacion: lectura. El modo de fallo peligroso **no es alcanzable** (defensa en profundidad).

Evidencia:

- `reconcile_egress_nat:4037` corre `delete_egress_nft_table_if_present()?` antes de `nft -f` (`:4039-4040`), y `egress_nft_ruleset` no incluye `delete table`/`flush` propio, asi que el pre-delete separado hace la operacion no transaccional.
- Pero los unicos callers son bootstrap (`:790`, fatal/crash-loop loud en egress) y el watchdog (`:1294`). El watchdog solo invoca `reconcile_egress_nat` dentro de `if !nft_table_loaded()` (`:1292`): cuando la tabla ya esta ausente. En ese estado `delete_egress_nft_table_if_present` (`:3962-3965`) hace `if !nft_table_loaded() { return Ok(()) }` = no-op. Asi el watchdog nunca borra una tabla sana presente.

Impacto:

La no-atomicidad es real como observacion de calidad, pero el "silent partial en host vivo" no se materializa: en el watchdog el delete es no-op guardado y el ciclo recurrente reintenta; en bootstrap el fallo es loud. El unico estado peligroso requiere una carrera TOCTOU extrema entre `:1292` y `:3963`.

Recomendacion:

Prepender `delete table inet fluxbee_egress` dentro del propio fichero `-f` (transaccion implicita de nft) para volverlo atomico y eliminar el pre-delete separado. Mejora de robustez.

## Gaps de cobertura (no auditado en profundidad)

- **Parsing del payload v2**: `resolve_add_hive_role`, `resolve_add_hive_egress_section`, `resolve_add_hive_dist_sync_probe_timeout_secs`, coercion de `address` (`unwrap_or_default`), rango/limites de `dist_sync_probe_timeout_secs`/`harden_ssh`/`restrict_ssh`. Solo se reviso `hive_id` (F3) y parcialmente las ifaces (F7/F12/F13).
- **Persistencia/atomicidad de `info.yaml`**: `write_hive_info`/`read_hive_info`/`append_*_history` y si una escritura parcial puede dejar un `hive_partial` inconsistente (central para F3/F9).
- **Senales/shutdown**: `shutdown_sequence` (`:1329`) duerme 10s fijos y no interactua con operaciones `add_hive`/`remove_hive` en curso; comportamiento ante SIGTERM a mitad de bootstrap SSH o `remove_dir_all`.
- **Observabilidad**: `append_*_history` registra `ok` aunque haya SSH fallback (F1/F2). Conviene auditar que los caminos degradados queden trazables.
- **Clasificacion de errores de red**: `is_socket_only_unreachable_error` (`:14586`) decide caer-a-bootstrap vs `FINALIZE_FAILED` (F2) por substring matching; merece test dedicado.
- **Bootstrap egress por password**: `add_egress_hive_flow` usa `ssh_with_pass_any` como canal primario y siembra key con `apply_remote_unrestricted_authorized_key_with_pass`; ese camino password-first y su diferencia con el worker no se audito.

## Cobertura faltante de tests

- Concurrencia: `add_hive`/`remove_hive` sobre el mismo `hive_id` (o dos `add_hive` paralelos) que detecte el TOCTOU `hive_partial_exists` -> `remove_dir_all` -> `create_dir_all` (F18).
- Traversal: `hive_id` y `address` invalidos no deben disparar `hive_partial_exists`/`remove_dir_all`/`create_dir_all` (F3); cubrir worker **y** egress.
- Inyeccion: `wan_iface`/`lan_iface` con metacaracteres de shell/nft/YAML deben ser rechazados en `resolve_egress_nat_config` (F7/F12/F13).
- Allowlist (F4): con `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin`, `WF.orch.diag@<hive>` y `SY.wf-rules@<hive>` deben ser rechazados. **Requiere modificar el test existente `:18391`** (que hoy asierta lo contrario), no solo agregar el caso negativo.
- Cross-hive (F15): `SY.admin@<hive-ajeno>` debe ser rechazado.
- Gate ADMIN (F8): `ADMIN_COMMAND` desde origen no permitido -> FORBIDDEN; positivo desde `SY.admin@<hive>`.
- `add_hive` socket-only (F2): si `socket_only_ready=true`, cualquier fallo de probe/finalize termina sin tocar bootstrap SSH; test de `is_socket_only_unreachable_error`.
- `remove_hive` (F1): unit que garantice que online responde `socket_ok`/`socket` exacto (sin SSH) y offline `local_only`; correr `orchestrator_remove_hive_socket_e2e.sh` en lab para cerrar el loop.
- Idempotencia (F9): reintento sobre un hive `pending` debe completar el finalize, no devolver `HIVE_EXISTS`.
- Egress (F5/F16): unit del shape del payload vs spec §9 (separado del E2E de reachability de dos NICs); `conntrack_tuned` debe distinguir live de pending-reboot.
- Drift (F6): con `nft_table_loaded()=true` pero contenido alterado, el reconcile debe re-aplicar.
- panic-safety (F14): un panic en `watchdog_tick` no debe dejar `watchdog_running=true` permanente.

## Orden sugerido de fixes

1. **Validar `wan_iface`/`lan_iface`** en `resolve_egress_nat_config` (cierra F7 RCE + F12 nft + F13 YAML de un solo golpe) y reescribir `write_remote_file` para no construir shell por interpolacion.
2. **Gate de origen en `ADMIN_COMMAND`** (F8).
3. **Validar `hive_id` al inicio** de `add_hive`/`add_egress` + canonicalizar deletes (F3), y **lock por `hive_id`** (F18).
4. **Quitar SSH de `remove_hive`** (F1) y **bloquear fallback SSH** una vez elegido socket-only en `add_hive` (F2).
5. **Hacer `add_hive` idempotente** (F9): reintento sobre `pending` completa el finalize.
6. **Hardening SSH egress fatal** (F10) y **teardown de NAT en `remove_hive` egress** (F11).
7. **Allowlist efectiva y same-hive** (F4 + F15): sin prefijos hardcodeados, match exacto `name@hive`.
8. **panic-safe watchdog** (F14) + **clamp de slices SHM** (F20).
9. **Verificacion/persistencia egress** (F5 internet_reachable, F16 conntrack, F17 carga al boot, F6 drift).
10. Bajas restantes (F19, F21, F22, F23, F25) segun capacidad.

## Falsos positivos descartados

Verificados adversarialmente y descartados como no-bug (se documentan para que no reaparezcan):

- **`remove_hive` local-only con worker offline devuelve `status:ok`**: es la degradacion local-only deliberada y documentada del contrato v2 (`sy_orchestrator_v2_tasks.md:21`, E2E-3), con gate E2E dedicado que exige justamente `status:ok` + `remote_cleanup_via=local_only` y senal explicita via `remote_cleanup`. La unica observacion legitima (UX: el `ok` top-level puede ocultar la degradacion; no hay reconciliacion de workers huerfanos) es materia de charla de diseno, no un bug de codigo.
- **Fallo de syncthing peer-link/dist_sync tras finalize exitoso no revierte el finalize remoto**: no produce doble-provision ni inconsistencia. Tanto el finalize del worker (`compute_local_core_update_sets`) como el peer-link de la mother (`reconcile_syncthing_peer_xml`) son idempotentes; el reintento entra por el fast-path socket idempotente, no por bootstrap SSH; `hive_partial_exists`+`remove_dir_all` limpia el dir local vacio. Residuo real: observabilidad (no hay campo `worker_already_finalized`), cosmetico.
