# FINDINGS — hallazgos del despliegue integrado en PROD alpha

> **Qué es esto.** El registro acumulado de lo que la prueba integrada en producción va sacando a la
> luz. La bitácora diaria (`YYYY-MM-DD.md`) cuenta *el viaje*; este documento junta *los hallazgos*
> para que después salga un **plan de cambios de código** fundamentado.
>
> **Regla que lo gobierna** (`METHOD.md` §3, reglas 0b/0c): durante el despliegue **no se toca código
> y no se parchea para que ande**. Un fallo es **dato**, no obstáculo. Todo lo que aparezca acá se
> discute con el operador **antes** de convertirse en un cambio.

**Estados:** 🔴 confirmado en código · 🟡 observado (falta confirmar causa) · 🟢 resuelto/no requiere cambio

---

## A. Producto fluxbee — candidatos a cambio de código

### A-1 🟡 REENCUADRADO (→ PB-5) — El orchestrator no configura placas de red secundarias

> **Reencuadrado en la auditoría del 2026-08-03 ([PB-5](PENDING-BUGS.md#pb-5)):** direccionar las
> placas es del operador por contrato, en todos los roles. Queda abierto que el egress reporta
> `nat_applied: true` sin verificar su pata LAN.

- **Qué pasa:** `add_hive role=ingress|egress` **exige** los nombres de interfaz (`wan_iface`,
  `lan_iface`) y asume que **ya existen y están direccionadas**. No hay una sola línea en el
  orquestador que asigne IPs a interfaces.
- **Evidencia:** `src/bin/sy_orchestrator.rs` — `resolve_add_hive_egress_section` valida
  `lan_cidr`/`wan_iface`/`lan_iface`; `reconcile_egress_nat` aplica nftables **sobre interfaces que
  supone configuradas**. Cero manejo de direcciones.
- **Impacto:** todo despliegue con ingress/egress necesita configuración de red **manual o externa**
  (en este caso, cloud-init). En bare-metal sin cloud-init, alguien la hace a mano → paso no
  reproducible, fuera de la receta.
- **Detectado por:** el operador lo anticipó; confirmado leyendo el código.
- **Estado:** hueco real del producto. **A discutir:** ¿el orchestrator debería aceptar la
  configuración de las patas secundarias en el payload de `add_hive` y aplicarla?

### A-2 🟡 ABIERTO (→ PB-6) — `harden_ssh` viene en `false` por defecto

> **Reescrito en la auditoría del 2026-08-03 ([PB-6](PENDING-BUGS.md#pb-6)):** invertir el default
> dejaría cajas sin ninguna vía de acceso. Lo abierto es que el canal `ssh_password` no se cierra
> solo.

- **Qué pasa:** con bootstrap por `ssh_password`, si no se pasa `harden_ssh:true` explícitamente,
  al terminar el join `add_hive` **saca su clave y su sudoers pero deja `PasswordAuthentication yes`**.
  La máquina queda con password abierto.
- **Evidencia:** `resolve_add_hive_harden_ssh` → default `false`. El endurecimiento
  (`disable_remote_password_auth_with_access` + verificación) solo corre si está en `true`.
- **Impacto:** el modelo mental correcto es *"la caja se abre unos segundos y `add_hive` la cierra"*.
  Con el default actual **eso no se cumple** salvo que el operador se acuerde del flag.
- **Estado:** **a discutir.** Opciones: invertir el default, o hacerlo ruidoso (advertir en la
  respuesta cuando se bootstrapeó con password y no se endureció).

### A-3 ✅ CERRADO (→ PB-7, `9f86c12`) — El timeout del admin (180 s) puede quedar corto para `add_hive`

> **Cerrado con [PB-7](PENDING-BUGS.md#pb-7):** `add_hive` responde `202` enseguida y el join corre en
> segundo plano, con su fase en `info.yaml`. Validado en vivo el 2026-08-05.

- **Qué pasa:** `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` default **180 s**, pero las esperas internas del
  flujo pueden sumar más (30 s salud + 60 s WAN + 60 s LSA + finalize).
- **Evidencia:** `src/bin/sy_admin.rs` (timeout) vs. los gates de `add_hive_flow` en
  `sy_orchestrator.rs`.
- **Mitigación existente:** el hive queda en `status: pending` y **reintentar es idempotente**.
- **Impacto:** en cajas lentas el cliente ve *timeout* aunque el join siga y termine bien →
  confunde y puede inducir a "arreglar" algo que estaba andando.
- **Estado:** **a discutir.** ¿Subir el default, o que la respuesta indique explícitamente
  "en progreso, reintentá para ver el estado"?

### A-4 ✅ CERRADO (→ PB-8) — `egress.gateway_ip` se propaga a los workers, pero **no a motherbee**

> **Cerrado con [PB-8](PENDING-BUGS.md#pb-8):** no era un olvido. `egress.gateway_ip` es, por
> contrato, la salida de los workers, y propagarlo al motherbee lo dejaría sin plano de control. El
> motherbee lo reporta con `EGRESS_MOTHERBEE_BYPASS` y no toca su ruta. Validado en vivo.

- **Qué pasa:** cuando MB declara `egress.gateway_ip`, **cada worker** rutea su default por el egress
  (`reconcile_worker_egress` → `ip route replace default via <gw>`). **MB no está en esa lista.**
- **Impacto:** el tráfico saliente de **motherbee** —que es justamente quien llama a las APIs
  externas (OpenAI, Slack, Meta)— **no sale por el nodo egress**, sino por su propia ruta por
  defecto. En este despliegue eso significa que saldría por la red de administración, que es
  justo lo que el diseño quiere evitar.
- **Estado:** **observado en el código, sin confirmar en vivo todavía.** Es de los puntos a mirar
  cuando el egress esté funcionando. **A discutir:** ¿es intencional (MB debe tener su propia
  salida) o es un hueco?

### A-5 🟡 Un clon recién booteado **no está listo** para `add_hive`

- **Qué pasa:** el primer arranque de una VM clonada de la imagen cloud dispara actualizaciones
  automáticas (`unattended-upgrades`), **reinicia sola**, y durante ese rato el `qemu-guest-agent`
  (y potencialmente sshd) quedan intermitentes.
- **Medido:** `netin` 37 MB → 52 MB, `diskwrite` 2.5 GB, reinicio espontáneo, agente caído varios
  minutos.
- **Impacto:** correr `add_hive` "apenas bootea la VM" puede pegarle a una caja en pleno upgrade →
  falla intermitente, difícil de diagnosticar, y encima combinado con **A-3** (timeout).
- **Estado:** observado en este despliegue. **A discutir:** ¿`add_hive` debería tener un *readiness
  gate* explícito (esperar a que la caja esté quieta) o alcanza con documentarlo en la receta?

### A-6 ✅ RESUELTO — `build-deb.sh` **nunca compilaba `ai_node_runner`** (divergencia con `install.sh`)

- **Qué pasa:** el paso `[1/5] build rust` de `packaging/build-deb.sh` hace:
  ```bash
  cargo build --release --bins
  cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
  cargo build --release --manifest-path nodes/io/Cargo.toml -p <io crates>
  ```
  y un comentario afirma: *"ai.generic (nodes/ai) is a root workspace member already built by
  `--bins`"*. **Eso es falso.**
- **Por qué:** el `Cargo.toml` raíz tiene **`[package]` (json-router) Y `[workspace]`**. Con un
  paquete raíz presente, `cargo build --bins` **sin `--workspace`** compila **solo los bins del
  paquete raíz**. `nodes/ai/ai-generic` (paquete `fluxbee-ai-nodes`, bin `ai_node_runner`) es un
  **miembro** → **nunca se compila**. Nótese que `sy-frontdesk-gov` —otro miembro— **sí** tiene su
  línea explícita: el patrón correcto ya está aplicado ahí, a `ai-generic` se le pasó.
- **Síntoma:** el build llega a `[3/5] stage files` y aborta:
  `Error: binary does not exist: target/release/ai_node_runner` → **no se produce `.deb`**.
- **Por qué nunca se notó:** en el build box del **lab** el binario ya existía en `target/release/`
  porque un `build2.sh` manual previo **sí** lo compila explícitamente
  (`cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner`). `build-deb.sh` lo encontraba
  y seguía. **En una caja limpia —como prod— falla.**
- **Impacto:** el camino de build **canónico** no es autosuficiente. Cualquier build box nuevo
  (o un `cargo clean`) rompe la construcción del `.deb`.
- **Arreglo propuesto (una línea, a aprobar):** agregar junto a la de `sy-frontdesk-gov`:
  ```bash
  cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner
  ```
- **Lo que lo convierte en divergencia (no en decisión de diseño):** **`scripts/install.sh` línea 246
  SÍ tiene** `cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner`. Y `base-nodes.json`
  declara explícitamente que es *"the single source of truth read by BOTH packaging/build-deb.sh and
  scripts/install.sh (**they must not diverge**)"*. O sea: `install.sh` estaba bien y `build-deb.sh`
  era el outlier → **de-divergir, no inventar**.
- **✅ RESUELTO** (aprobado por el operador): se agregó a `build-deb.sh` la **misma línea** que ya
  tenía `install.sh`, y se **corrigieron los dos comentarios falsos** que afirmaban que `--bins` lo
  construía (esos comentarios son la razón por la que el bug sobrevivió). **Los dos scripts quedaron
  con paridad exacta** en los tres builds explícitos.
- **Atajo rechazado:** el propio error sugiere `-buildvcs=false` para B-8 y aquí habría bastado
  compilar el binario a mano — ambas cosas habrían hecho pasar el build **ocultando el problema** y
  dejando la próxima caja limpia rota igual.

### A-7 ✅ RESUELTO (0.1.41–0.1.42) — OPA de usuario no llegaba a los spokes (CONFIG_CHANGED nunca salía del motherbee)

- **Qué pasa:** el admin manda las escrituras OPA como CONFIG_CHANGED; el router lo intercepta
  antes de rutear, ignora el destino y solo lo reparte dentro del host. Un clear dirigido a worker1
  lo ejecutó el motherbee (2026-09-28). Ningún spoke tiene de dónde sacar la policy; ingress y
  egress ni siquiera corren SY.opa.rules. Y el wasm compilado (140–155 KB) no entra en un mensaje
  (tope de 128 KiB).
- **Arreglo (diseño del operador, bitácora 2026-09-30; `docs/opa-distribution.md`):** SY.opa.rules
  en todos los hives; solo el motherbee compila y publica el wasm en la carpeta Syncthing
  `fluxbee-dist-policy`; un CONFIG_CHANGED `{opa, sync}` avisa; cada hive lo instala si el sha256
  coincide; una sola policy global; convergencia eventual; sin cifrado. CONFIG_CHANGED pasa a
  viajar por el ruteo normal (cruza hives).
- **Validado en 8.x:** apply en los 4 hives en 16 s; un hive cortado de Syncthing se puso al día
  solo, 27 s después de reconectar; clear global (lento hasta 0.1.43: A-19).

### A-8 ✅ RESUELTO (0.1.36) — Config de un hive aplicada en otro: `add_route` en un spoke pisaba las rutas del motherbee

- **Confirmado en vivo** en 8.x (bitácora 2026-09-30): dos altas en worker1 y el motherbee quedó
  con esas rutas. El broadcast posterior a cada alta/baja llevaba la lista del spoke, y el contador
  del admin iba uno adelante del de SY.config.routes.
- **Arreglo:** `ConfigChangedPayload.hive`; el admin lo completa y SY.config.routes / SY.opa.rules
  ignoran la config de otro hive.

### A-9 ✅ RESUELTO (0.1.36) — Cualquier nodo podía reescribir rutas, taps u OPA de su hive

- **Qué pasaba:** CONFIG_CHANGED no era acción protegida y el intercept del router lo repartía sin
  mirar el origen; SY.config.routes aceptaba `add_route`/`add_tap` de cualquiera (el tipo `admin`
  no pasa por el gate del router); SY.opa.rules aceptaba `compile/apply/rollback_policy` de
  cualquiera. Un nodo de usuario podía, por ejemplo, agregarse un tap y espejar tráfico.
- **Arreglo:** CONFIG_CHANGED protegido (router + `system.rego`, reglas vigentes, sin recompilar);
  mutaciones de SY.config.routes y comandos de SY.opa.rules solo desde `SY.admin@motherbee`.

### A-10 ✅ RESUELTO (0.1.36) — Nodos fuera del motherbee apuntaban a `SY.vault@<su hive>` / `SY.admin@<su hive>`

- **Qué pasaba:** vault y admin corren solo en el motherbee. Cognition, los runners AI y los IO
  derivaban el destino del vault de su propio hive; SY.wf-rules publicaba vía `SY.admin@<su hive>`.
  En un worker, nada de eso existe. SY.edge ya lo había resuelto con `vault_hive`.
- **Arreglo:** `VaultClient::for_primary` y `PRIMARY_HIVE_ID` en el SDK (el router la re-exporta);
  `PrimaryAdminNode` en el Go SDK para SY.wf-rules.

### A-11 ✅ RESUELTO (0.1.36) — La réplica de identity no se ponía al día al reconectar

- **Qué pasaba:** el full sync corría solo al arrancar. Si el stream de deltas se cortaba, o el
  worker arrancaba con el motherbee caído, la réplica quedaba con datos viejos (o solo los ILK de
  sistema) hasta el próximo reinicio, aunque el log decía que iba a converger al reconectar.
- **Arreglo:** cada suscripción empieza con un snapshot completo; un gap de secuencia reconecta y
  resincroniza en vez de `exit(0)`.

### A-12 ✅ RESUELTO (0.1.37) — El primer spawn de un WF en un worker corría antes que Syncthing

- **Qué pasaba:** con el publish arreglado (A-10), el deploy llegaba al spawn antes de que el
  paquete sincronizara a worker1 (~11 s medidos) → `RUNTIME_NOT_AVAILABLE`, un solo intento.
- **Arreglo:** wf-rules reintenta (backoff 1→4 s, hasta 20 s) mientras el orquestador conteste
  `RUNTIME_NOT_AVAILABLE` / `RUNTIME_NOT_PRESENT` / `BASE_RUNTIME_NOT_AVAILABLE`.

### A-13 ✅ RESUELTO (0.1.37) — El SDK tapaba el veredicto del vault

- **Qué pasaba:** `VaultClient::get`/`list` parseaban la respuesta como éxito antes de mirar el
  `status`; un `KEY_NOT_FOUND` llegaba como `json error: missing field key` y las ramas
  `KEY_NOT_FOUND` de io-slack e io-wapp nunca corrían.
- **Arreglo:** leer el veredicto del JSON crudo antes de parsear.

### A-14 ✅ RESUELTO (0.1.37) — La SHM de OPA era `0666`

- **Qué pasaba:** SY.opa.rules creaba `/dev/shm/jsr-opa-<hive>` `rw-rw-rw-` (y la volvía a poner
  así si existía): cualquier proceso podía leer la policy o escribir una para el router. Sus pares
  Rust usan `0600`. Archivos de policy `0644` en dirs `0755`.
- **Arreglo:** región `0600` (se corrige al arrancar), archivos `0600`, dirs `0700`.

### A-15 ✅ RESUELTO (0.1.38–0.1.40) — wf-rules borraba paquetes del espejo `dist/` de un worker

- **Qué pasaba:** tras apply/rollback/delete, wf-rules purgaba versiones y reescribía el manifest
  en su `dist/runtimes` **local**. En un worker eso es un espejo Syncthing receive-only: el borrado
  local no se restaura nunca. En worker1 quedaron 15 cambios locales (13 borrados); el paquete
  `wf.w1probe` desapareció del espejo y el nodo WF entró en crash-loop por
  `flow/definition.json` faltante. Además el orquestador aceptaba el spawn con el manifest ya
  sincronizado pero sin los archivos del paquete.
- **Arreglo:** en un hive que no es el motherbee, la purga pide `remove_runtime_version` al admin
  (se borra en el origen y la sync lo lleva); el orquestador responde `RUNTIME_NOT_PRESENT` hasta
  que el `package.json` del paquete esté en el hive. En 0.1.38 el pedido salía sin `target`
  (`INVALID_REQUEST: missing target`); 0.1.39 lo dirige al motherbee; 0.1.40 purga dejando la
  versión current al final y toma `RUNTIME_NOT_FOUND`/`RUNTIME_VERSION_NOT_FOUND` como hecho.
- **Validado (0.1.40):** crear → re-aplicar → borrar un WF en worker1: nodo HEALTHY en 0.0.1 y
  0.0.2 (18 s cada uno), delete OK, motherbee sin versiones ni entrada en el manifest, espejo de
  worker1 con 0 cambios locales.
- **Reparación puntual:** el espejo de worker1 se restauró con `POST /rest/db/revert` de Syncthing
  (15 → 0 cambios locales); después de eso `WF.w1probe@worker1` quedó HEALTHY: el primer WF
  corriendo en un worker.

### A-22 🟡 PARCIAL (0.1.44) · resto POSTERGADO — Un nodo local podía mandar mensajes como otro nodo

- **Qué pasaba:** el router buscaba al emisor de un frame por el `routing.src` que el frame declaraba
  y estampaba desde ahí su nombre L2, que es con lo que el gate decide las acciones protegidas.
  Confirmado en vivo en 8.x: un nodo de prueba en el motherbee mandó `NODE_STATUS_GET` (protegida)
  con el UUID de SY.admin y SY.config.routes le respondió al admin; con su propio UUID, el gate lo
  descartó. Los UUID viajan en todos los mensajes (un nodo de tenant ve el de su orquestador en cada
  señal `node_config`), así que cualquier nodo local podía actuar como el admin del motherbee o como
  el orquestador de su hive, que por la regla 3 puede todas las acciones protegidas en cualquier hive.
- **Arreglo:** el router descarta el frame si `routing.src` no es el UUID con el que ese socket hizo
  HELLO, que es lo que `02-protocolo.md` ya exigía ("el router estampa `src_l2_name` desde la sesión
  autenticada"). Revisados los emisores Rust, Go e IO: todos usan su propio UUID.
- **Validado:** el mismo probe ahora cae (`routing.src is not the sending node`); ningún descarte de
  tráfico legítimo en los 4 hives.
- **Lo que sigue abierto (panel DTAP 2026-10-01, P-1):** 0.1.44 cerró que un nodo use el UUID de otro
  dentro de su conexión, pero el **nombre** lo declara el propio proceso en el HELLO y el router lo
  acepta: socket `0666`, sin nombres reservados, sin rechazo de duplicados ni de un `@hive` ajeno.
  Cualquier proceso local puede conectarse con un UUID nuevo como `SY.admin` y escribir la policy
  global, o como `SY.orchestrator` en un spoke.
- **Decisión del operador (2026-10-01): POSTERGAR** el paquete de seguridad (hoy cuesta más de lo
  que rinde). Incluye: autenticar el nombre en el HELLO (P-1), dirección de la regla 3 (A-20 / D-2),
  firma del manifest publicado (P-8), `get_policy` que expone el rego (P-16) y el cifrado de la copia
  sincronizada (ya postergado en el diseño).

### A-23 🟡 RESUELTO (0.1.48) · varias soluciones SIMPLIFICADO, revisar — El arquitecto modelaba OPA por hive; hay una sola policy global

- **Qué pasa (visto en código, 2026-10-01):** una solución declara `opa_deployments` por hive
  (`hive`, `policy_id`, `rego_source`) y el plan emite un `opa_compile_apply` por cada uno.
  - Si el hive no es el motherbee, el admin lo rechaza con 400.
  - Si es el motherbee, la policy pasa a ser la de todos los hives. Dos despliegues se pisan.
  - El snapshot busca `policy_id`/`rego_hash` en el estado, que no los tiene: el reconciliador
    nunca ve la policy que corre y la re-aplica en cada corrida.
  - Quitarla está bloqueado (no hay acción de remove).
  - El plan compiler (IA) recibe solo el `delta_report`: nadie le pasa el rego declarado.
- **Decisión del operador (2026-10-01):**
  1. La solución declara una sola policy global, sin hive.
  2. Se compara con el rego que corre en el motherbee; si es igual, no se hace nada.
  3. Se aplica con un solo `opa_compile_apply`; un hive `pending` no es una falla.
  4. Cuando la solución dueña deja de declararla, `opa_clear`.

  Si dos soluciones declaran OPA, **gana la última**. "Dueño" es la solución cuya policy corre
  ahora; se reconoce comparando el rego con los manifests guardados. Una solución solo hace clear
  si la policy que corre es la suya, y el plan dice de quién es la policy que reemplaza.
- **Arreglo (0.1.48, `1486807`):** `desired_state.opa` (una policy, sin hive); el snapshot lee la
  policy del motherbee con `opa_get_policy` y el dueño de los manifests guardados; un solo
  `opa_compile_apply`, y `OPA_REMOVE` = `opa_clear`; el plan compiler recibe el rego y los pasos OPA
  del plan quedan fijados a él; la confirmación dice de quién es la policy que se reemplaza.
- **Validado:** tests (177 del arquitecto, incluida la respuesta real del admin de 8.x). El
  pipeline completo no se corrió en vivo: necesita la IA del arquitecto (A-26).
- **Pendiente de revisar (simplificación consciente, como el paquete de seguridad):** componer las
  reglas de varias soluciones en una sola policy, en vez de que gane la última; y un dueño
  registrado junto con la policy, en vez de deducirlo comparando el rego.

### A-24 🟡 RESUELTO (0.1.49) · WF pendiente — El snapshot del arquitecto le pedía el estado de cada hive a un admin que no existe

- **Qué pasa (visto en código, 2026-10-01; no reproducido en vivo):** `build_actual_state_snapshot`
  manda cada lectura a `SY.admin@<hive>`. Solo el motherbee tiene admin (en worker1,
  `sy-admin` está inactivo), así que en una solución con más de un hive esas lecturas fallan, el
  snapshot queda incompleto y el reconciliador bloquea toda la corrida.
- **Arreglo (0.1.49):** el snapshot y la consulta `query_hive` del plan compiler le preguntan a
  `SY.admin@motherbee`, con el hive como destino (el admin está y va a estar solo en el motherbee;
  operador, 2026-10-01). Un test del código fuente impide volver a nombrar un admin por hive.
- **Sigue abierto:**
  - El plan compiler tampoco recibe la definición de los workflows declarados
    (`wf_deployments`): el mismo hueco que A-23 tenía con el rego.
  - En ingress y egress, `list_runtimes` responde `RUNTIME_MANIFEST_MISSING` y no hay SY.wf-rules
    (es así por rol). Una solución que nombra un hive DMZ igual queda con el snapshot incompleto,
    y eso bloquea la corrida. En worker1 las cinco lecturas dan ok.

### A-25 🟡 RESUELTO (0.1.49) — El `.deb` no instalaba el handbook del arquitecto (`install.sh` sí)

- **Qué pasa (2026-10-01):** el arquitecto agrega a sus prompts `/etc/fluxbee/handbook_fluxbee.md`
  si existe. `install.sh` lo copia, `build-deb.sh` no: en el motherbee de 8.x no está, así que el
  arquitecto de PROD trabaja sin handbook. Misma clase de divergencia que A-6.
- **Arreglo (0.1.49):** `build-deb.sh` lo instala en el mismo lugar (documento estático: se
  reemplaza en cada upgrade, no es conffile).

### A-26 🟡 RESUELTO (0.1.49) — El arquitecto leía sus secretos del vault una sola vez al arrancar

- **Qué se vio (2026-10-01, al instalar 0.1.47 y 0.1.48):** al arrancar, el arquitecto no pudo leer
  su clave de IA ni la URL de su base de mensajes (`vault unreachable`): en un upgrade se reinicia
  todo el hive y el vault todavía no contestaba. Quedó escuchando con `ai_configured=false` y
  `messages_db_configured=false`.
- **Corrección del diagnóstico:** el aviso de arranque del vault (`VAULT_SECRET_CHANGED`, uno por
  secreto) **sí** llegó: la base de mensajes se reconectó 0,4 s después. La IA sigue apagada porque
  el vault de 8.x no tiene clave de IA (tiene 3 secretos: edge, TLS y postgres). Aun así, ese aviso
  sale una sola vez, y el SDK documenta que un nodo que no lo escucha a tiempo queda degradado
  (así le pasó a SY.storage).
- **Arreglo:** como SY.storage y SY.identity, las dos lecturas de arranque reintentan hasta que el
  vault conteste (`resolve_resource_awaiting_vault`, hasta `VAULT_BOOT_WAIT`, las dos a la vez).
  Las relecturas por aviso o por `CONFIG_SET` siguen siendo de una vez.

### A-27 ✅ RESUELTO (`802faf5`) — El guardián de paridad del catálogo del admin (CI) estaba en rojo desde antes

- **Qué pasa (2026-10-01):** `scripts/admin_action_catalog_parity_check.sh` corre en GitHub Actions
  en cada push que toca `sy_admin.rs` y falla: sus expresiones no reconocen cómo se despachan
  hoy 20 acciones que sí tienen ruta HTTP o son solo internas: `get_runtime`,
  `list_cloud_actions`, `publish_artifact`, `publish_cloud_endpoint`, `unpublish_artifact`, los 8
  `timer_*` y los 8 `wf_rules_*`. Cada push del tramo OPA que tocó el admin lo disparó en rojo.
- **Arreglo:** el script sigue también `handle_admin_query_with_payload`, las RPC de timer, el
  despacho interno genérico, los handlers de wf-rules y el de cloud endpoint, y declara las
  excepciones a propósito: acciones que solo llegan por la malla (`publish_artifact`,
  `list_cloud_actions`) y acciones con brazo dedicado (`externalize`, `unexternalize`,
  `list_externalized`); las dos listas fallan si quedan viejas. En CI pasa desde `802faf5`.

### A-28 ✅ RESUELTO (`3eeadf2`) — Tres binarios compilados de Go estaban versionados en git

- `go/sy-opa-rules/sy-opa-rules`, `go/sy-timer/sy-timer` y `go/nodes/wf/wf-generic/wf-generic`.
  Ensuciaban cada `git status` después de un build local y quedaban a un `git add -A` de un commit.
- **Fix:** salen del índice (los archivos quedan en disco) y `.gitignore` ignora los cuatro que
  escribe el build (también `sy-wf-rules`). Aprobado por el operador en el lote de temas chicos
  (2026-10-02).

### A-29 ✅ RESUELTO (0.1.51) — sy-timer y wf-generic: SQLite esperaba el lock en una sola conexión

- **Qué pasaba:** el primer CI de Go mostró un test de sy-timer inestable (20 `TIMER_SCHEDULE`
  concurrentes con el mismo `client_ref`): fallaba 10 de 15 veces en fb-build con
  `TIMER_STORAGE_ERROR: database is locked (SQLITE_BUSY)`. La causa era del producto:
  `busy_timeout` (y `synchronous` en wf-generic) es un pragma **por conexión**, y se aplicaba con
  `db.Exec` sobre el pool de `database/sql`. Solo una conexión esperaba el lock; las demás fallaban
  al instante. En sy-timer el scheduler escribe mientras se atienden pedidos, así que un agendado o
  un disparo podía fallar de forma intermitente en producción.
- **Arreglo:** los pragmas van en el DSN (`ruta?_pragma=...`), que modernc.org/sqlite aplica a
  cada conexión que abre. Tests nuevos verifican tres conexiones a la vez; la suite de sy-timer
  pasó 15 veces seguidas con `-race`.

### A-30 ✅ RESUELTO (0.1.51) — El admin no decodificaba los `%xx` de la query

- **Qué pasaba (al validar 0.1.50):** un `DELETE /hives/{h}/taps` con `@` enviado como `%40` (como
  lo manda `urlencode` de Python) respondía `NOT_FOUND`: el admin comparaba el valor sin decodificar.
- **Arreglo:** claves y valores de la query se decodifican; `+` queda literal (aquí son más comunes
  los `+` crudos, como `Etc/GMT+3`) y un `%` suelto se conserva.

### A-31 🟡 Los servicios escriben códigos de color ANSI en el journal

- **Qué pasa:** los niveles salen como `\x1b[33m WARN\x1b[0m` en `journalctl`. Un `grep ' WARN '`
  no encuentra nada, y al validar 0.1.52 un conteo de errores dio 0 en falso hasta limpiar los
  códigos con `sed 's/\x1b\[[0-9;]*m//g'`.
- **Por qué:** cada binario arma su `tracing_subscriber` por su cuenta (36 archivos) y ninguno
  apaga el color cuando la salida no es una terminal.
- **Arreglo propuesto:** un init de tracing en el SDK con `with_ansi` solo si la salida es una
  terminal, usado por todos los binarios. Es un cambio mecánico en 36 archivos: va con la pasada
  de formato masivo (ítem 8 del lote), a decisión del operador.


### A-32 ✅ RESUELTO (0.1.53) — Datos personales en logs y respuestas (frontdesk e identity)

- **Qué pasaba:**
  - SY.frontdesk.gov escribía en el log, a nivel INFO, el payload de `ILK_REGISTER` (nombre, email,
    teléfono, empresa), el nombre de empresa que tipeó la persona (`TNT_CREATE`), los primeros 240
    caracteres de cada mensaje y el id del remitente (en WhatsApp, su teléfono).
  - Los errores de SY.identity viajaban con su texto libre. Para un email duplicado ese texto trae el
    DETAIL de Postgres con el email.
  - SY.identity logueaba el payload de `ILK_PROVISION` (la dirección de la persona) y devolvía el
    DETAIL de Postgres a quien llamaba.
- **Arreglo:**
  - De un registro se loguea `ilk_id`, `tenant_id` y qué campos vinieron, nunca sus valores.
  - De un error de identity, solo su código.
  - SY.identity loguea `ich_id` y `channel_type`, y responde el mensaje de Postgres sin el DETAIL.
  - Tres tests capturan los logs de las tres entradas del frontdesk; con los campos viejos fallan.

### A-33 ✅ RESUELTO (0.1.53) — SY.frontdesk.gov: rechazos definitivos de identity reportados como reintentables

- **Qué pasaba:** los errores se clasificaban buscando texto. TENANT_PENDING, ILK_DELETED,
  SYSTEM_ILK_PROTECTED, DUPLICATE_* y otros terminaban como `IDENTITY_UNAVAILABLE` con
  `retryable: true`.
  - **Además:** la respuesta del target de fallback no se revisaba. Un `status:"error"` salía como
    `registered: true`, un REGISTERED falso. Solo podía pasar con `GOV_IDENTITY_FALLBACK_TARGET`
    configurado.
- **Arreglo:**
  - El bridge devuelve el `IdentityError` del SDK, igual que el orchestrator, y el código de
    SY.identity se conserva tal cual.
  - Solo es reintentable lo transitorio: sin respuesta, NOT_PRIMARY, DB_NOT_READY y DB_WRITE_FAILED.
  - Las dos respuestas pasan por el mismo chequeo de estado.

### A-34 ✅ RESUELTO (0.1.53) — Cognition: el arranque en frío nunca corría y reconstruía mal

- **Qué pasaba (visto en PROD y en una revisión del código):**
  - **El rebuild nunca corría.** La credencial de Postgres llega del vault después del arranque y el
    rebuild solo se intentaba al iniciar.
  - **Reconstruía con otras claves.** Indexaba por id de entidad mientras el camino en vivo indexa
    por etiqueta. Después de un reinicio, el siguiente turno duplicaba contextos, razones y memorias
    con el mismo id.
  - **El estado en vivo no se podaba nunca.**
  - **Un scope no se cortaba jamás:** un cambio de tema sostenido lo renombraba en vez de cortarlo.
  - **`storage.enabled: true` estaba fijo** en CONFIG_GET.
  - **El texto determinístico de los episodios nunca se enviaba.**
- **Arreglo:**
  - **Rebuild:**
    - Espera al vault al arrancar (el patrón de A-26, hasta 60 s).
    - Si igual no carga, lo reintenta el primer turno, una sola vez y dentro de la tarea de turnos.
    - Arma el estado con las mismas claves que el camino en vivo y solo se instala sobre un estado
      vacío.
    - Los episodios recuperan su instancia real.
  - **Poda:** el estado en vivo se recorta al mismo conjunto que entra en la SHM `jsr-memory`.
  - **Scopes:** un cambio de tema sostenido corta el scope; uno que todavía liga es deriva y lo
    reetiqueta. El dominante se elige de forma determinística.
  - **CONFIG_GET:** informa `storage.db_configured` real.
  - **Episodios:** su texto es el del resumidor de IA.
  - Tests nuevos para cada caso; cada uno se verificó fallando sin su arreglo.

### A-35 ✅ RESUELTO (0.1.53) — ai.generic pedía la clave de IA por nombre

- **Qué pasaba:** desde julio (`ai-engine-selection.md` D2/D3), un nodo `AI.*` tenía que nombrar su
  clave (`behavior.vault_key`). Sin esa clave, su config se rechazaba.
- **Decisión del operador (2026-10-02, D4):** *"La clave de IA la debería tomar del vault con la
  clave general (sin asignación de nombre)."*
- **Arreglo:**
  - El proveedor es el del hive (`hive.yaml` `ai`) y la clave es la general de ese proveedor en
    SY.vault: la del tenant del nodo y, si no hay, la del raíz. Es lo mismo que hacen los SY.* y el
    frontdesk.
  - `behavior.vault_key` se rechaza con un mensaje que lo nombra.

### A-36 ✅ RESUELTO (0.1.53) — La instalación base levantaba una instancia de prueba y Archi conocía runtimes inexistentes

- **Qué pasaba:**
  - `AI.chat@motherbee`, el nodo de ejemplo de marzo, arrancaba con cada instalación como si fuera
    base.
  - Las semillas del cookbook le decían a Archi que los paquetes `config_only` usaran
    `runtime_base: "AI.common"`, que no existe.
  - El handbook que carga Archi decía que `ai.chat` es un runtime.
  - Un ejemplo del architect lanzaba un nodo sin runtime, que se habría derivado a `ai.chat`.
  - Una ayuda del architect tenía UTF-8 codificado dos veces ("catÃ¡logo").
- **Arreglo:**
  - `ai.generic` queda horneado y sin instancia de arranque. Las instancias se crean para algo
    concreto; las de prueba son efímeras (operador, 2026-10-02).
  - Todo apunta a `ai.generic`: admin, architect, handbook, semillas, docs y scripts.
  - Se borraron `install-ia.sh` y `ai-nodectl.sh` (el modelo de unit systemd por nodo de marzo) y
    los flags deprecados de los scripts de publicación.
- **PROD:** la instancia `AI.chat@motherbee` que ya existe queda en FAILED_CONFIG, porque su config
  tiene `vault_key`. Se borra o se reconfigura a pedido del operador.

### A-37 ✅ RESUELTO (0.1.54 + 0.1.55) — SY.frontdesk.gov: el camino de tenants y el merge por email

**Decisiones del operador (2026-10-02):**

- Sobre tenants: *"no da para crear un tenant por un mensaje que llegue de un humano que no esté
  registrado; ahora el tenant se crea desde el cloud y tiene un proceso"*.
- Sobre el merge: *"haría merge sobre datos que no están… si no coinciden los datos y mandan un update
  queda lo último"*.

**Tenant del caso:**

- Es el del ILK temporal, que SY.identity registró al provisionarlo bajo el tenant del nodo IO. El
  frontdesk lo lee del SHM de identity.
- El `tenant_id` de un handoff solo tiene que coincidir; si no, `INVALID_REQUEST`.
- Sin tenant legible responde `TENANT_UNRESOLVED` y no registra.
- El frontdesk ya no crea tenants: SY.identity le quitó el permiso de `TNT_CREATE`, `TNT_UPDATE` y
  `TNT_SET_SPONSOR`.
- Ni la persona ni el LLM eligen el tenant.

**Estado del tenant:** `ILK_REGISTER` en un tenant suspendido ahora se rechaza (`TENANT_SUSPENDED`),
igual que en uno pendiente.

**Merge por email**, dentro de `ILK_REGISTER` de SY.identity:

- **Cuándo:** el email ya es de otro humano activo del mismo tenant y el que se registra es un
  temporal de ese tenant.
- **Qué hace:** los canales se mueven (antes se copiaban, un bug) y el temporal queda como alias.
- **Los datos:** solo se completan los campos vacíos; nunca se pisa un valor.
- **La respuesta:** `merged:true`; el frontdesk responde `MERGED`.
- **Gana lo último:** un `ILK_REGISTER` del propio ILK, por ejemplo un `register_human` repetido,
  reemplaza la identificación.
- **io.cloud:** `register_human` informa el ILK final, `merged` y el temporal de origen.

**Además:**

- Las búsquedas por email son por tenant, como el índice de la base.
- Un handoff fallido conserva el tenant del caso (G7).
- Los tests de logs sin datos personales eran inestables; quedaron estables.

**Cerrado en 0.1.55:**

- **Tenant raíz:** nadie se registra ahí. Decisión del operador (2026-10-02): *"nadie se puede
  registrar en tenant raíz"*.
  - SY.identity rechaza el `ILK_REGISTER` de una persona en el tenant raíz con
    `TENANT_ROOT_NOT_REGISTRABLE`, venga de quien venga. Los ILKs de nodos (`agent`) se siguen
    registrando ahí.
  - El frontdesk corta antes, sin LLM ni SY.identity, y responde `TENANT_NOT_REGISTRABLE`. La
    persona queda temporal.
  - Los nodos IO lanzados desde el tenant raíz quedan como A-43.
- **El gate de frontdesk de io.slack e io.wapp** (`io-common`) armaba el handoff como mensaje
  `data`, pero el frontdesk solo toma handoffs de mensajes `user`. El handoff nunca llegaba a su
  camino determinístico. Ahora va como `user`, igual que el de io.cloud.
- **Los e2e de laboratorio de identity:** cuatro diags creaban sus tenants con el nombre del
  frontdesk y recibían `UNAUTHORIZED_REGISTRAR`. Eran `identity_merge`, `identity_negative`,
  `identity_provision_complete` e `identity_replica_sync`.
  - Ahora el script crea los tenants por SY.admin, se los pasa al diag y los purga al final. Lo común
    quedó en `scripts/lib/identity_e2e.sh`.
  - El de réplica además comprueba por SY.admin que el tenant y el ILK llegan a la réplica.
  - El wrapper `gov_frontdesk_identity_e2e.sh` ya no trae hives viejos (`sandbox`, `worker-220`).
- **Los diags se hacen pasar por el frontdesk:** se conectan con su nombre y con el UUID que el
  frontdesk persistió.
  - Mientras corren, el router les entrega el tráfico del frontdesk.
  - Cuando se van, el frontdesk real queda conectado pero sin ruta hasta que reconecta (A-44).
  - Los scripts ahora paran `sy-frontdesk-gov` mientras corre el diag y lo vuelven a arrancar.

**Quedan abiertos:**

- 🔴 **Seguridad:** no hay prueba de que el email sea de quien lo escribe (A-42, postergado).
- **G6 y G8:** los contadores de §12 y los reintentos conversacionales.

### A-38 ✅ RESUELTO (0.1.54) — Cognition: preguntas de diseño

Respuesta del operador (2026-10-02): *"sí a todo"*.

- **Umbrales:** `context_open`/`reason_open` pasan a llamarse por lo que hacen,
  `config.thresholds.context_close` / `reason_close`, con default 0,25 (el mismo efecto que antes).
  CONFIG_SET rechaza las claves viejas por nombre y cualquier otra clave desconocida. La config
  persistida de PROD con las viejas arranca con los defaults, que dan el mismo comportamiento, y
  deja un warning.
- **Región llena:** gana lo reciente. El hilo del turno actual nunca se descarta y después va el
  visto más recientemente.
- **Corte de scope:** se mantiene en el 6.º mensaje divergente sostenido (5.º si el scope tiene un
  turno, 7.º si está muy reforzado), escrito en la spec §8.3 y fijado por test.
- **Sigue sin ensamblar** (capítulo aparte): memoria entre hilos, LanceDB, que lo de los workers
  llegue al motherbee, y lectores de las tablas.

### A-39 ✅ RESUELTO (0.1.54, a validar en el deploy) — En cada release, los nodos administrados de motherbee caían entre 20 y 80 s (`203/EXEC`)

- **Qué pasa (visto al validar 0.1.53, y preexistente: AI.chat sumó 104 fallas en 30 días):**
  1. Al desempaquetar la versión nueva, dpkg borra los archivos del paquete viejo, y los directorios
     `dist/runtimes/<rt>/<versión vieja>` venían en ese paquete.
  2. Durante el update las units de los nodos administrados se reinician con el `ExecStart` viejo,
     que apunta a un directorio que ya no existe: loop de `status=203/EXEC`.
  3. Recién cuando arranca el orchestrator nuevo, ve que `current` se movió y las reapunta.
- **Evidencia 0.1.53:**
  - AI.chat, IO.api, IO.blob, IO.cloud e IO.slack.default: 5 fallas entre 20:32:10 y 20:32:31.
    Reapuntadas a las 20:32:34 ("runtime 'current' pointer moved; rebinding node").
  - IO.wapp.default: 17 fallas, hasta las 20:33:33; el reconcile la había salteado como "visible".
- **Impacto:** IO.api e IO.cloud, lo que da la cara al público, quedan sin servicio en cada upgrade.
- **Quién los reiniciaba:** `needrestart`, el hook de apt de Ubuntu. Después de dpkg reinicia los
  servicios que corren binarios borrados, y su `ExecStart` viejo apunta al directorio que ya no
  existe.
- **El otro lado del bug:** el reconcile de arranque salteaba los nodos que estaban corriendo.
  Uno que nadie reiniciaba quedaba en el binario viejo indefinidamente; los reparaba solo porque
  `needrestart` los había hecho caer.
- **Arreglo (`779df4c`):**
  - El reconcile de arranque, que corre después de cada upgrade y cada core-update, relanza en la
    versión nueva al nodo que sigue a `current` cuando el puntero se movió: un reinicio controlado.
    El loop de 60 s no lo hace, así que una publicación en caliente se comporta como antes.
  - El paquete instala `/etc/needrestart/conf.d/fluxbee.conf`, que deja las units `fluxbee-node-*`
    al orchestrator (verificado contra el código de `needrestart`).


### A-40 ✅ RESUELTO (0.1.55) — IO.slack.default no tenía su ILK en identity (PROD)

- **Qué pasaba:** su config apuntaba a `ilk:9cd1bb44…`, pero SY.identity respondía `NOT_FOUND`, así
  que cada `CONFIG_SET` fallaba al registrar su propio canal y el nodo quedaba en FAILED_CONFIG.
  Probablemente lo borró la primera corrida del factory reset del 28/09.
- **Decisión del operador (2026-10-02):** *"io.slack es parte del sistema pero funciona
  instanciándose, no corre solo… corre con su propio tenant que lo lanza"*.
- **Cómo se cerró:**
  - `io.slack` queda horneado y sin instancia de arranque en `base-nodes.json`;
  - cada binding lo lanza su tenant;
  - la instancia de PROD, que corría en el tenant raíz, se borró con purga. Su config quedó guardada
    fuera del repo para recrear el binding desde un tenant.

### A-41 ✅ RESUELTO (0.1.54) — Purgar un nodo dejaba su UUID persistido

- **Qué pasaba:** `purge_instance` borraba el directorio, el ILK y el mapeo, pero no
  `state/nodes/<nombre>.uuid`. En PROD había 12, de nodos ya borrados: AI.chat, los de prueba del
  factory reset y bindings viejos de Slack. Un nodo nuevo con el mismo nombre heredaba el UUID viejo.
- **Arreglo (`7c9f781`):** las dos rutas de purga lo borran y lo informan (`uuid_file_removed`). Se
  borraron los 12 de PROD.


### A-42 🔴 POSTERGADO (paquete de seguridad) — El merge por email no verifica que el email sea de quien lo escribe

- **Qué pasa (desde 0.1.54, A-37):** si alguien escribe el email de otra persona del mismo tenant,
  SY.identity hace el merge y su canal queda asociado al ILK de esa persona. Lo enrutan como si fuera
  ella y puede completar los campos que estén vacíos.
- **Decisión del operador (2026-10-02):** *"levántalo y dejamos para después"*. Va con el paquete de
  seguridad (A-22).
- **Arreglo previsto:** una prueba de pertenencia antes del merge, por ejemplo un código de un solo
  uso al email.

### A-43 ✅ RESUELTO (0.1.56) — Nodos IO lanzados desde el tenant raíz

- **Qué pasaba:** la instalación base levantaba nodos IO de canal en el tenant raíz
  (`IO.api@motherbee`, `IO.wapp.default@motherbee`). Una persona que llegaba por uno de ellos quedaba
  temporal, porque desde 0.1.55 nadie se registra en el tenant raíz.
- **Decisión del operador (2026-10-02):** *"algunos nodos IO tienen que correr con el tenant raíz,
  ej io.cloud io.blob los demás no"*; *"todos quedan para poder usarlos pero no corriendo default.
  Solo io.cloud y io.blob por ahora"*.
- **Arreglo (`4a9249f`):**
  - El SDK define `ROOT_TENANT_IO_RUNTIMES` (io.cloud, io.blob).
  - El orchestrator rechaza cualquier otro nodo IO en el tenant raíz con `TENANT_ROOT_NOT_ALLOWED`
    en los tres lugares donde lanza un nodo:
    - `run_node`, que cubre admin, Cloud, Archi, firstboot y los reenvíos;
    - start/restart;
    - el relanzamiento al arrancar. Si un nodo ya corre, lo deja corriendo.
  - io.api e io.wapp quedan horneados y sin instancia de base; firstboot solo levanta IO.cloud e
    IO.blob.
  - Archi nunca elige el tenant raíz para un nodo IO.
  - Docs: `715143a`.
- **PROD:** el operador purgó `IO.api@motherbee` e `IO.wapp.default@motherbee` antes del deploy de
  0.1.56; desde la sesión lo bloqueaba el clasificador de permisos. En 30 días no habían tenido uso y
  sus configs quedaron guardadas fuera del repo.
- **Validado en vivo (0.1.56):** un `run_node` de io.api en el tenant raíz da
  `TENANT_ROOT_NOT_ALLOWED` y no deja nada creado.
- **io.web (operador, 2026-10-03):** *"io.web también va en el core, la idea es que los IO que se
  requieren como recursos compartidos y no porque una solución lo requiera, corren en el core"*.
  - Quedó en `ROOT_TENANT_IO_RUNTIMES`, aunque todavía no existe: hay solo una spec.
  - Su spec lo declara en `system_nodes`; cuando se construya, va como IO.blob e IO.cloud (runtime
    managed `boot=true`).
- **El runbook de LinkedHelper de Noelia** lanza el nodo en el tenant raíz. Lo ve el operador.

### A-44 ✅ RESUELTO (0.1.55 + 0.1.56) — Dos conexiones con un mismo UUID: la segunda se queda con la ruta

Visto al arreglar los e2e de A-37, leyendo `src/router/mod.rs` y `src/shm/mod.rs`.

- **Qué pasaba en el router:**
  - Un HELLO con un UUID que ya tenía una conexión viva reemplazaba su entrada en la tabla de
    nodos.
  - Cuando la nueva se cerraba, la vieja quedaba conectada pero sin ruta.
- **Decisión del operador (2026-10-02):** *"rechazar la nueva, no veo porque deba aparecer una en el
  esquema normal"*.
- **Lo que mostró verificarlo antes:**
  - **Bug preexistente:** si una conexión terminaba por error (de lectura, un OPA_RELOAD
    inválido, un reenvío a pares, `handle_message`), el router no la limpiaba. La entrada quedaba en
    la tabla y el socket abierto: un nodo vivo seguía conectado sin que nadie leyera sus mensajes.
    Con "rechazar la nueva" eso se volvía un bloqueo permanente.
  - **Duplicados pasajeros en el esquema normal:**
    - todo nodo Go manda dos HELLO al arrancar;
    - un nodo puede reconectar antes de que el router procese el cierre de la conexión anterior.
- **Arreglo (`068014d`, 0.1.56):**
  - Toda conexión termina limpiando su entrada; un mensaje que no se puede procesar se loguea y se
    saltea.
  - Un HELLO con un UUID ocupado espera hasta 1 s a que la conexión anterior se vaya; si sigue viva,
    se rechaza sin ANNOUNCE y con WARN. El chequeo y la inserción van bajo un mismo lock.
  - El SDK loguea el handshake rechazado.
  - El diag negativo de identity usa una conexión por nombre.
  - Los e2e de identity corren con `timeout`: sus reintentos esperaban un error que ningún binario
    imprime.
  - Tres de los cuatro tests nuevos fallan con el código anterior.
- **SHM del router (0.1.55):**
  - `register_node` duplicaba a un nodo que se registraba de nuevo.
  - El lector perdía el último nodo cuando había un hueco.
- **Quedan anotados:**
  - el doble HELLO del SDK de Go: lo absorbe la espera, pero el SDK podría reusar la primera
    conexión;
  - el SDK de Rust no tiene timeout de handshake: un router que acepta y no contesta lo deja
    colgado.

### A-45 ✅ RESUELTO (0.1.56; se ve desde 0.1.57) — Cada upgrade del motherbee apaga todo el core antes de desempaquetar

- **Qué pasa (medido en el deploy de 0.1.55):**
  - El `prerm` del paquete (`packaging/deb-prerm`) para todos los servicios del core antes de
    desempaquetar, también en un upgrade.
  - El `postinst` arranca solo el orchestrator, que después levanta el resto.
- **Caída medida:** rt-gateway 70 s, sy-admin 76 s, sy-vault 78 s, sy-storage 84 s, sy-identity
  86 s.
- **Impacto:** mientras tanto el motherbee no tiene router, identity ni admin.
  - IO.api e IO.cloud no atienden: el público ve 502.
  - Las réplicas de identity reintentan cada segundo (~2 avisos por segundo).
  - No se puede provisionar a nadie.
- **Por qué está así:** el `prerm` dice "stop + disable services before removal", pero corre igual
  en un upgrade.
- **Propuesta:** que el `prerm` pare los servicios solo al desinstalar, y que en un upgrade el
  `postinst` los reinicie después de desempaquetar. Linux mantiene corriendo el binario viejo
  hasta el reinicio, así que la caída quedaría en lo que tarda cada reinicio.
- **A decidir:** el orden de los reinicios, y si los hace el `postinst` o el orchestrator.
- **Operador (2026-10-02):** *"no me parece mal"*.
- **Arreglo (`1a10cab`):**
  - En un upgrade el `prerm` para solo a los que escriben el manifest de runtimes:
    sy-orchestrator, sy-admin y sy-wf-rules. El `postinst` cuenta con eso al registrar los runtimes.
  - El resto sigue sirviendo con los binarios viejos.
  - El boot del orchestrator nuevo reinicia cada servicio que corre un binario reemplazado, en su
    orden de arranque: rt-gateway primero y SY.vault último. Así ordenar los reinicios lo resuelve el
    orchestrator, no un script.
  - El resultado queda en `/versions` como `core.last_update`, con `via: package`.
  - needrestart deja también rt-gateway y sy-* al orchestrator.
  - dpkg corre el `prerm` del paquete viejo: el upgrade a 0.1.56 todavía para todo, y la mejora se
    ve desde 0.1.57.

### A-19 ✅ RESUELTO (0.1.43) — La policy publicada esperaba hasta 60 s al watcher de Syncthing

- **Qué pasaba:** con 0.1.41 el apply llegaba a los 4 hives en 16 s, pero el clear tardó 63 s y el
  admin respondió con los tres spokes pendientes. Los eventos de Syncthing del motherbee lo
  muestran: los dos applies se escanearon 10 s después de publicar (`fsWatcherDelayS`), el clear
  60 s después. El watcher retiene hasta su timeout (6 × 10 s) los cambios que son renames y
  borrados, y la publicación escribe por rename. Con 0.1.42 retuvo también un apply: a los 33 s
  solo el motherbee corría la v8.
- **Arreglo:** después de aplicar en el motherbee, el admin le manda al orquestador del motherbee
  un sync hint de `fluxbee-dist-policy`, que escanea la carpeta en el momento (el mismo paso que el
  publish de un runtime), y recién entonces el aviso (0.1.42). En 0.1.42 el orquestador lo rechazaba
  (`folder_id` inválido para el canal `dist`: solo aceptaba `fluxbee-dist`, aunque el handler ya
  sabía honrar una carpeta puntual) → 0.1.43 acepta cualquier `fluxbee-dist*`.

### A-20 🟡 PARCIAL (0.1.42 + 0.1.45) · dirección POSTERGADA — Con CONFIG_CHANGED cruzando hives, sus receptores aplicaban cualquier origen que el router admitiera

- **Qué pasaba:** el gate del router para CONFIG_CHANGED admite al admin primario **y a los
  orquestadores** (regla 3 de la policy de sistema). SY.opa.rules aplicaba un compile/apply/clear de
  cualquiera de ellos, y lo que se aplica en el motherbee ahora se publica a todos los hives.
  SY.config.routes aplicaba la lista de rutas/VPN/taps de un CONFIG_CHANGED global, cuyo único
  emisor era `PUT /config/*` (eliminado en 0.1.41): ahora llegaría a todos los hives y les pisaría
  la config.
- **No es capacidad nueva:** la regla 3 ya deja a un orquestador escribir la policy o las rutas de
  cualquier hive por `CONFIG_SET`. El orquestador no lo usa: `CONFIG_SET`/`CONFIG_GET` los manda
  solo SY.admin; el orquestador reenvía entre hives SPAWN/KILL/NODE_CONFIG_*/NODE_STATUS/LIST_NODES/
  GET_*/ROLLBACK/ADD_HIVE_FINALIZE/REMOVE_HIVE_CLEANUP y manda CONFIG_CHANGED `node_config` solo a
  nodos de su hive. **Decidido (operador, 2026-10-01): achicarla** → 0.1.45: la regla 3 es la
  lista de las 16 acciones que el orquestador reenvía, y se borraron las señales de config que nadie
  usaba (`node_config` + `notify`, el `CONFIG_RESPONSE` de storage, el handler de sy-wf-rules, la
  rama de io-slack). Validado: ciclo de WF en worker1 y lecturas entre hives sin un solo rechazo del
  gate.
- **Lo que NO cierra (panel DTAP, D-2):** la lista no tiene dirección. El orquestador de cualquier
  hive, incluido el del ingress, todavía puede mandarle a otro hive las mutaciones de la lista
  (`NODE_CONFIG_SET`, `SPAWN`/`KILL`/`RESTART`/`REMOVE`, `SYSTEM_CORE_ROLLBACK`, finalize/cleanup). Y
  mientras el nombre del HELLO no se autentique (A-22), cualquier proceso local puede presentarse
  como orquestador. Postergado con el paquete de seguridad (A-22).
- **Arreglo:** SY.opa.rules toma CONFIG_CHANGED solo de `SY.admin@motherbee`, igual que su camino de
  comandos y que el protocolo; SY.config.routes ya no aplica listas que lleguen por CONFIG_CHANGED.

### A-21 ✅ RESUELTO (0.1.50) — Estado del router sin policy de usuario: `error` al arrancar, `ok` después de un clear

- **Qué se ve:** en `/hives/{h}/opa/status`, un router que arrancó sin policy reporta
  `load_status=1, error`; después de un clear, `load_status=0, ok`. En los dos casos no hay policy
  y el ruteo es el mismo (un mensaje sin destino que no va al frontdesk sale `OPA_ERROR`).
- **Causa:** el resolver nace en `OPA_STATUS_ERROR` y `unload()` lo deja en `OPA_STATUS_OK`. Es solo
  informativo.
- **Arreglo (0.1.50):** el resolver nace en el mismo estado que deja un clear (`ok`, sin policy).

### A-17 ✅ RESUELTO (0.1.39) — Una réplica de identity que arranca sin primario borra los ILK de su hive

- **Qué pasaba:** la réplica re-publica cada 30 s su conjunto de ILK propios y el primario lo toma
  como autoritativo (borra los ILK del hive que no vienen). Si el worker arranca con el primario
  inalcanzable, en memoria solo tiene sus ILK de sistema: al volver la conexión publicaba eso y el
  primario **borraba los ILK de los nodos del hive**. Visto en 8.x: el primario pasó de 25 a 24
  ILKs y `AI.vaultprobe@worker1` quedó sin identidad (`ILK_NOT_FOUND` al matarlo). Es el caso de
  un reboot completo en el que un worker levanta antes que el motherbee.
- **Arreglo:** el conjunto propio no se publica hasta tener una base del primario (full sync de
  arranque o el primer snapshot de la suscripción). Los deltas locales sueltos siguen saliendo.
- **Validado:** mismo arranque degradado → la réplica loguea `holding its self-owned snapshot`; el
  primario sigue en 25 ILKs, con el del probe.

### A-18 ✅ CERRADO (0.1.50) — config-routes: rutas estáticas, VPN y taps

- **La necesidad (operador, 2026-09-30):** las rutas estáticas se usan poco y pueden quedar
  locales. La VPN va a usarse de forma global (separar tenants, seguridad: un nodo de worker1
  habla con uno de worker2). Los taps son la herramienta más usada (echo a otros nodos) y tienen
  que ser globales. Decidido no meterlas en OPA: se arreglan en config-routes.
- **Cómo está hoy (verificado en código, 2026-10-01):**

  | Herramienta | Se carga | Llega al resto | ¿Global? |
  |---|---|---|---|
  | Rutas estáticas | en un hive | LSA → FIB de todos los routers | sí |
  | Taps | en un hive | LSA; el router de origen aplica los locales y los de LSA, una sola vez | sí |
  | VPN | en un hive | LSA las transporta, pero `assign_vpn` usa solo la config local | **no** |

- **Decisión del operador (2026-10-01): la VPN queda por hive.** Cada router la asigna con las
  reglas de su hive; para una VPN entre hives se carga la misma regla en cada uno (documentado en
  `06-regiones.md` §5.1). Con eso no queda nada abierto; *"resolvé los detalles por
  costo/beneficio... prefiero empezar lo próximo con estos temas cerrados"*.
- **Cerrado en 0.1.50 (`aa206ac`):**
  - Una sola superficie: `/hives/{h}/routes|vpns|taps`. Se borraron `/routes`, `/vpns` y `/taps`
    con `?hive=` (nadie los usaba).
  - La regla 5 de `system.rego` ya no nombra `SY.config-routes`.
  - El router relee la config de ruteo en cada latido, con control de versión (panel D-12): un
    router sin nodos locales ya no queda con config vieja. Hoy no aplicaba (un router por hive).
- **Aceptado:**
  - Si el hive donde se cargó una ruta o un tap queda cortado, deja de aplicarse en los demás
    durante el corte.
  - Los taps matchean por nombre exacto (v1).
  - En un host con varios routers, el router par no hace el fanout de taps (mismo caso que D-12).
- **Ya resuelto:**
  - `PUT /config/*`: 0.1.40–0.1.41.
  - Broadcast posterior a cada alta y trato especial del router a CONFIG_CHANGED: 0.1.44.
  - Broadcast de storage: 0.1.41; su CONFIG_RESPONSE: 0.1.45.
  - Config con alcance de hive y control de origen: 0.1.36 (A-8, A-9).
  - Taps cross-hive con destino por UUID: julio (`18c4dd8`).
- **Aceptado:** si se borra o se reconstruye un hive, se pierden sus rutas, taps y VPN (nadie
  guarda copia).

### A-16 ✅ RESUELTO (0.1.56) — Los espejos receive-only acumulan cambios locales en silencio

- **Qué pasa:** en una carpeta receive-only, Syncthing no propaga ni revierte los cambios locales.
  Si algo toca el espejo en un spoke, diverge del motherbee para siempre y nadie se entera. El
  orquestador administra Syncthing pero no mira ni revierte estos cambios.
- **A discutir:** que el orquestador revierta (`/rest/db/revert`) los cambios locales de las
  carpetas receive-only y lo loguee, para que el espejo converja solo al origen.
- **Arreglo (`10c97b1`):**
  - El watchdog, que ya leía el estado de cada carpeta en cada vuelta, lee también
    `receiveOnlyTotalItems`.
  - Si una carpeta receive-only tiene cambios locales, deja un WARN y la revierte.
  - Cubre los espejos de `dist/` de los spokes y el `public/` de blob del ingress.
  - El 2026-10-02 las 12 carpetas receive-only de PROD estaban en 0.
- **Validado en vivo (0.1.56, 2026-10-03, con el OK del operador):**
  - Un archivo de prueba en `dist/vendor` de worker1 subió `receiveOnlyTotalItems` a 1 a los 11 s.
  - El watchdog dejó el WARN y revirtió la carpeta.
  - A los 16 s el archivo ya no estaba y el contador volvió a 0.

---

## B. Infraestructura y herramientas — no requieren cambio de código del producto

### B-1 🔴 La API de Proxmox no permite escribir *snippets* de cloud-init

- Ni `POST /nodes/{n}/storage/{s}/upload` ni `download-url` aceptan `content=snippets`
  (enum: `iso, vztmpl, import`), aunque el storage **sí** admite el tipo.
- **Impacto:** **no se puede construir un template 100 % por API**; hace falta que el operador
  deposite el archivo una vez. Es el límite Nivel-3 de `METHOD.md` §2 en acción.
- **Mitigación aplicada:** el operador lo escribió una sola vez; el template quedó con el agente
  horneado y **los clones ya no dependen del snippet**.

### B-2 🔴 `cicustom` reemplaza el user-data de Proxmox (anula `ciuser`/`cipassword`)

- Al usar `cicustom=user=...`, Proxmox usa ese archivo **en lugar** del user-data que genera, que es
  el que crea el usuario. Resultado: el usuario **no se crea** y el bootstrap de `add_hive` no tendría
  con quién entrar.
- **Regla para el handbook:** *o* `cicustom` *o* `ciuser`/`cipassword`; **no conviven**.

### B-3 🟢 La imagen cloud de Ubuntu es qcow2 aunque se publique como `.img`

- `download-url` rechaza `filename=*.img` (`invalid filename or wrong extension`). Se descarga
  renombrando a `.qcow2`.

### B-4 🟢 Ubuntu 24.04 usa **activación por socket** para sshd

- `ssh.socket` activo escuchando en `:22`, `ssh.service` inactivo. **Es normal** y no afecta a
  `add_hive` (que solo necesita el puerto 22 respondiendo). Anotado para no diagnosticar mal.

### B-5 🟢 Las operaciones de VM en Proxmox se serializan por *lock*

- Lanzar `resize`/`start` inmediatamente después de un `qmcreate` que importa una imagen grande
  falla con `can't lock file … got timeout`. **Error propio cometido y corregido.**
- **Regla:** encadenar operaciones por **estado de task** (`/tasks/<UPID>/status` → `stopped OK`),
  nunca por `sleep`.

### B-7 🟢 El guest-agent de Proxmox ejecuta **sin `HOME`** definido

- **Qué pasa:** `agent/exec` corre el comando sin `HOME` en el entorno. Con `bash -lc`, eso hace que
  `/root/.profile` evalúe `. "$HOME/.cargo/env"` como `. "/.cargo/env"` → error en **cada** comando.
- **Impacto real (no cosmético):** ese ruido en `stderr` **contaminó la salida de todos los comandos**
  y **rompió dos pollers propios** que extraían números de la salida (el mensaje contiene `line 10`).
  Dos falsos positivos: el problema **no era la VM, era el helper**.
- **Solución:** invocar `/usr/bin/env HOME=/root /bin/bash -lc '<cmd>'` en el helper de guest-agent.
- **Regla para el handbook:** el canal guest-agent **no es una shell de login normal** — definí
  `HOME` explícitamente, y **nunca parsees números de una salida que puede traer stderr**.

### B-8 🟢 Clonar como un usuario y compilar como otro rompe el build de Go (`buildvcs`)

- **Qué pasó:** el repo se clonó como `fluxops` (para usar la deploy key) pero `build-deb.sh` corre
  como `root`. Git rechaza el repo ajeno (`fatal: detected dubious ownership in repository`) y el paso
  Go falla al estampar la información de VCS:
  ```
  error obtaining VCS status: exit status 128
      Use -buildvcs=false to disable VCS stamping.
  ```
  El build llegó hasta `[2/5] build go` y salió con `rc=1` **sin producir `.deb`** — después de ~55 min
  de compilación Rust ya exitosa.
- **Causa:** error propio de setup (dos usuarios distintos para clonar y compilar), **no** un problema
  de fluxbee.
- **Solución aplicada (la estándar de git, no un atajo):**
  `git config --global --add safe.directory /opt/fluxbee` para root.
  *(La alternativa `-buildvcs=false` habría ocultado el problema y perdido el estampado de versión en
  los binarios Go: se descartó.)*
- **Regla para el handbook:** **el mismo usuario que clona debe compilar**, o declarar el repo como
  `safe.directory`. Verificarlo **antes** de lanzar un build largo.

### B-9 🟢 Proxmox sobre VMware: **la seguridad del portgroup rompe TODA red bridgeada de las VMs anidadas**

- **Síntoma:** las VMs con placa en `vmbr0`/`vmbr1` **no podían ni hacer ARP** a su gateway
  (`ARP INCOMPLETE`, 100 % packet loss), aunque el **host** Proxmox funcionaba perfecto en el
  **mismo bridge**, con IP/ruteo/carrier correctos del lado de la VM.
- **Causa:** el Proxmox es una VM de VMware. Los portgroups traen por defecto
  **`Forged transmits: Reject`** (+ `MAC address changes: Reject`, `Promiscuous mode: Reject`), lo que
  **descarta toda trama cuya MAC origen no sea la asignada a la vNIC del Proxmox**. Las VMs anidadas
  emiten con **su propia** MAC (`BC:24:11:…`) → VMware las tira.
- **Lo que hizo el diagnóstico concluyente** (y descartó fluxbee, firewall y configuración):

  | Observación | Explicación |
  |---|---|
  | ✅ el host Proxmox anda | usa **su** MAC asignada |
  | ✅ el worker sale a internet por `fbint`+SNAT | tráfico **ruteado** → sale con la **MAC del host** |
  | ❌ una VM en `vmbr0` no hace ni ARP | tráfico **bridgeado** → **MAC de la VM** → descartado |

  Es decir: **la red SDN interna funcionaba justamente porque es routing con NAT, no bridging.**
  Esa asimetría fue la pista que cerró el caso.
- **Solución (operador, en ESXi, en caliente y por portgroup):**
  `Promiscuous mode: Accept` · `MAC address changes: Accept` · **`Forged transmits: Accept`** ← la que
  habilita **enviar**.
- **Detalle operativo caro:** hay **un portgroup por placa física**. Al aplicarlo solo al de `nic0`,
  `vmbr0` empezó a andar y `vmbr1` **siguió roto** — mismo síntoma, otra placa. **Hay que aplicarlo a
  TODOS los portgroups** de las NICs del hipervisor anidado.
- **Verificado después del cambio:** ingress con **IP pública propia `71.182.182.80` visible desde
  internet** + pata interna al mesh, simultáneas. Egress con salida por su pata WAN.
- **Para el handbook:** en cualquier prod sobre VMware, **esto se configura ANTES de desplegar**; si no,
  se pierden horas diagnosticando una red que del lado del guest está impecable.

### B-6 🟢 `build-deb.sh` podía producir un `.deb` truncado sin fallar

- Con el disco lleno, `dpkg-deb` salía con código 0 pero escribía un paquete de ~1.8 KB sin
  `data.tar`. **Ya corregido** (commit `01db2cc`): preflight de espacio + verificación de integridad
  del `.deb` con fallo ruidoso.

### B-10 🟢 El repo apt moría con cada reboot y se veía vacío durante cada publish

- **Qué pasaba (dos defectos en `scripts/apt-repo-publish.sh`):**
  1. `--serve` levantaba el server con `systemd-run` → unit **transitoria**: moría con cada reboot
     del build box, y sus errores se tragaban (`>/dev/null 2>&1 || true`), así que un re-run podía
     dejar el repo caído **sin avisar** (pasó en agosto, tras un reboot de VM110).
  2. El índice se regeneraba *in place* (`dpkg-scanpackages > Packages`): durante el re-hash de
     todos los `.deb` (~5,5 min con 33 × ~240 MB) el repo servía **0 paquetes** — un cliente que
     hiciera `apt-get update` en esa ventana veía el repo vacío.
- **Evidencia:** `FragmentPath=/run/systemd/transient/fluxbee-apt.service`; polls durante un publish:
  0 paquetes de 14:47 a 14:52 (bitácora 2026-09-25).
- **Solución:** `--serve` escribe una unit **persistente y habilitada** (idempotente, reemplaza una
  transitoria vieja, falla ruidoso si no arranca); el índice se arma en `.new` y entra por renames
  atómicos (`Release` último); el `.deb` se copia como `.partial` + `mv`. El one-liner imprime
  todas las IPs del box.
- **Validado en prod:** reboot de VM110 → repo arriba solo; publish real con 32 muestras desde un
  cliente → mínimo 33 paquetes.

### B-11 🟢 Un carácter por encima de U+00FF en `agent/exec` traba el guest-agent — era la "flakiness bajo carga"

- **Qué pasa:** la API de Proxmox (Perl) no codifica los argumentos de `guest-exec` con caracteres
  anchos: un solo `—` (U+2014), `→` o `✓` en el comando deja al **qemu-guest-agent trabado** —
  hasta `guest-ping` da timeout ("QEMU guest agent is not running") hasta que el agente se
  reinicia. Los caracteres Latin-1 (`é`, `ñ`, `¿`) pasan. `agent/file-write` con `encode=1` falla
  por lo mismo (`Wide character in subroutine entry`, sin trabar nada). Además, la salida vuelve
  con mojibake (bytes UTF-8 leídos como Latin-1: `—` → `â€”`).
- **Evidencia (A/B controlado, VM110):** con el agente sano, `exec echo acento-é` → OK y el agente
  sigue vivo; `exec echo guion—largo` → `guest-exec failed - got timeout` y el `ping` siguiente
  muere. Se descartaron carga (host cpu 0 %), disco (34 %) y kernel (6.8.0-142 ejecuta bien).
- **Impacto:** explica los episodios de "qga flaky" de toda la campaña de agosto (los comandos del
  agente llevaban `—`, `→`, `✓` en los `echo`) — **la causa no era la carga**. Cada uno costó
  reboots/resets de VMs de prod.
- **Solución (en `lab/pve.py`):** argv no-ASCII viaja en base64 y un bootstrap ASCII lo decodifica
  y ejecuta el argv original exacto; `push` codifica los bytes localmente (`encode=0`, también
  sirve para binarios); la salida se des-mojibakea. De paso se aplicó la solución de **B-7**
  (`env HOME=/root`), que no estaba en el helper. Validado en vivo: acentos/`—`/`✓`, exit codes
  propagados, el agente sobrevive.

### B-12 🟢 Todas las VMs de PROD tenían reboot pendiente por kernel + libc6 — aplicado

- **Qué se observó:** `unattended-upgrades` instaló kernels 6.8.0-137…142 y libc6 en las VMs; hasta
  que reinician siguen con el kernel viejo (fb-egress: corre 6.8.0-136, `reboot-required`). El
  reboot de VM110 de hoy activó **6.8.0-142** sin problemas (guest-agent, red y repo OK).
- **Resolución (2026-09-25; el operador delegó la infra: "hacé lo que creas conveniente"):**
  - Reboot de a una VM, con snapshot previo `pre-kernel-reboot-20260925`, en el orden egress →
    worker1 → ingress → mb.
  - Las 4 quedaron en **6.8.0-142**, sin `reboot-required`, con IPs y rutas idénticas y 0 units
    `failed`. Mesh 4/4, 9/9 runtimes, público 200.
  - Los spokes aguantaron la caída del hub sin reiniciar nada.
  - Quedó validada en PROD la carga al boot del NAT del egress (F17): el ruleset volvió idéntico.
  - Detalle en la bitácora 2026-09-25.

### B-13 ✅ RESUELTO (2026-10-02) — Las VMs de PROD no arrancaban solas después de un reboot del host (`onboot` sin definir)

- **Qué pasa:** `onboot` no está definido en fb-mb, fb-worker1, fb-ingress, fb-egress ni fb-build.
  Si el host reinicia (corte de luz, actualización), **PROD queda apagado** hasta que alguien
  arranque las VMs a mano.
- **Evidencia:** pasó el 2026-07-30 ("las 4 VMs quedaron apagadas. Arranqué las 4 VMs"). El HANDBOOK
  lo registró como "reboot del hipervisor: validado", pero lo validado fue que el mesh se rearma,
  no que las VMs arranquen solas.
- **Solución propuesta:** `onboot=1` en 100, 101, 102, 103 y 110 (fb-build, por la decisión de
  dejarla siempre encendida). Sin orden de arranque, porque el arranque simultáneo de las 4 es lo
  que se validó el 30/07. `PUT /nodes/pve/qemu/<id>/config onboot=1`, o en la GUI: VM → Options →
  *Start at boot*.
- **Estado:** resuelto. El operador puso `onboot=1` en 100, 101, 102, 103 y 110; verificado por la
  API el 2026-10-02. Queda por ver en vivo con el próximo reinicio del host.

### B-14 🟡 Arranque lento de las VMs de PROD (userspace de 53 s a 1 min 48 s)

- **Qué se observó (reboots del 2026-09-25):** userspace de ingress 53 s, egress 60 s, worker1
  89 s, mb 108 s (fb-build: 31 s).
- **Qué no es:**
  - No es el flush del journal, aunque `systemd-analyze blame` pone arriba a
    `systemd-journal-flush.service` (mb 74 s, worker1 58 s): journald reporta que el flush en sí
    tardó ~1 s (mb: 1,4 s para 752 entradas).
  - Tampoco el tamaño del journal: mb 1,8 G; egress 109 M y tardó 27 s; fb-build 112 M y 0,6 s.
- **Pista:** en el log de mb, jobs sin relación entre sí (flush, apparmor, binfmt) terminan en el
  mismo instante (94,6 s). Algo común los retiene.
- **Impacto:** suma de 1 a 1,5 min a cada recuperación. Con mb son ~3 min 20 s desde el reboot hasta
  los 9 runtimes; no diagnosticar antes de eso.
- **Candidato no probado:** la consola serie (`console=ttyS0` con `serial0=socket` y
  `vga=serial0`).
- **Pendiente:** diagnóstico (`systemd-analyze plot`, qué retiene esos jobs) antes de tocar nada.

### B-15 ✅ RESUELTO (2026-10-02) — Margen de memoria del host de PROD con fb-build siempre encendida

- **Qué se observó:** 26,0 G configurados en las VMs encendidas sobre 27,4 G físicos, sin
  ballooning. Uso real estable en ~22 G (pico de 81 % en la semana, 0 swap). mb llega a 5,8 G de
  sus 10 G; fb-build, a 7,6 G de 8 G.
- **Riesgo:** si mb llegara a usar sus 10 G mientras fb-build compila, el host se queda sin margen,
  y lo primero que cae es un proceso QEMU.
- **Opciones (decisión del operador):**
  - Ballooning con mínimo en fb-build (p. ej. `balloon=2048`): Proxmox le recupera memoria cuando
    el host pasa del 80 %.
  - Bajarle la RAM fuera de los builds.
- **Estado:** resuelto. El operador activó ballooning en fb-build con mínimo 2 GiB (`balloon=2048`);
  verificado por la API el 2026-10-02. Para darle más RAM al host hay que apagarlo, porque es una VM
  de VMware.

### B-16 ✅ RESUELTO (2026-10-03) — `ops.py deploy` dio por actualizados a spokes que no lo estaban

- **Qué pasó (deploy de 0.1.56):**
  - El core update se disparó unos segundos después de que el admin del motherbee volvió, antes de
    que su router tuviera de vuelta a los spokes. Los tres pedidos fallaron con `TRANSPORT_ERROR` y
    ninguno llegó: ningún servicio de los spokes se reinició.
  - Igual el deploy dijo "worker1 on 0.1.56". `ops.py` juzgaba por `core.manifest_hash` y las
    versiones de los componentes de `/versions`, que en un spoke son lo que trajo dist. Syncthing
    los actualiza antes de cualquier update. Lo instalado está en `core.installed`, y seguía en el
    hash de 0.1.55.
- **Cómo se vio:** el chequeo de binarios después del deploy mostró `not_restarted` en todas las
  units de los spokes.
- **Arreglo (`lab/ops.py`):**
  - `core_of` usa el hash instalado cuando el hive lo reporta, igual que la comparación de
    `/versions` (X1). `ops versions` dice de dónde sale cada hash.
  - `deploy` espera a que el orchestrator de cada spoke conteste antes del update, y reintenta hasta
    3 veces un pedido que no llegó.
  - El update se volvió a mandar a mano y los tres spokes terminaron bien (U-8b validado).
- **Para considerar:** en `/versions` de un spoke, `core.manifest_hash` y las versiones de los
  componentes describen lo que está disponible, no lo que corre. La comparación de la flota ya lo
  resuelve, pero leído suelto confunde.

---

## Cómo se usa este documento

1. Durante el despliegue: **se agregan hallazgos, no se arreglan**.
2. Al terminar: se revisa la sección **A** con el operador y sale el **plan de cambios de código**
   (qué se cambia, por qué, en qué orden, y qué queda como decisión de diseño). Ese plan se lleva a
   [`PENDING-BUGS.md`](PENDING-BUGS.md), que es donde se sigue el estado de cada tarea.
3. La sección **B** alimenta el `HANDBOOK.md` (recetas) y, donde corresponda, los scripts de infra.
