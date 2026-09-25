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

### A-1 🔴 El orchestrator no configura placas de red secundarias

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

### A-2 🔴 `harden_ssh` viene en `false` por defecto

- **Qué pasa:** con bootstrap por `ssh_password`, si no se pasa `harden_ssh:true` explícitamente,
  al terminar el join `add_hive` **saca su clave y su sudoers pero deja `PasswordAuthentication yes`**.
  La máquina queda con password abierto.
- **Evidencia:** `resolve_add_hive_harden_ssh` → default `false`. El endurecimiento
  (`disable_remote_password_auth_with_access` + verificación) solo corre si está en `true`.
- **Impacto:** el modelo mental correcto es *"la caja se abre unos segundos y `add_hive` la cierra"*.
  Con el default actual **eso no se cumple** salvo que el operador se acuerde del flag.
- **Estado:** **a discutir.** Opciones: invertir el default, o hacerlo ruidoso (advertir en la
  respuesta cuando se bootstrapeó con password y no se endureció).

### A-3 🔴 El timeout del admin (180 s) puede quedar corto para `add_hive`

- **Qué pasa:** `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` default **180 s**, pero las esperas internas del
  flujo pueden sumar más (30 s salud + 60 s WAN + 60 s LSA + finalize).
- **Evidencia:** `src/bin/sy_admin.rs` (timeout) vs. los gates de `add_hive_flow` en
  `sy_orchestrator.rs`.
- **Mitigación existente:** el hive queda en `status: pending` y **reintentar es idempotente**.
- **Impacto:** en cajas lentas el cliente ve *timeout* aunque el join siga y termine bien →
  confunde y puede inducir a "arreglar" algo que estaba andando.
- **Estado:** **a discutir.** ¿Subir el default, o que la respuesta indique explícitamente
  "en progreso, reintentá para ver el estado"?

### A-4 🟡 `egress.gateway_ip` se propaga a los workers, pero **no a motherbee**

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

### B-13 🟡 Las VMs de PROD no arrancan solas después de un reboot del host (`onboot` sin definir)

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
- **Estado:** pendiente. **Lo aplica el operador**: el cambio de config del host no pasó el control
  de permisos del agente.

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

### B-15 🟡 Margen de memoria del host de PROD con fb-build siempre encendida

- **Qué se observó:** 26,0 G configurados en las VMs encendidas sobre 27,4 G físicos, sin
  ballooning. Uso real estable en ~22 G (pico de 81 % en la semana, 0 swap). mb llega a 5,8 G de
  sus 10 G; fb-build, a 7,6 G de 8 G.
- **Riesgo:** si mb llegara a usar sus 10 G mientras fb-build compila, el host se queda sin margen,
  y lo primero que cae es un proceso QEMU.
- **Opciones (decisión del operador):**
  - Ballooning con mínimo en fb-build (p. ej. `balloon=2048`): Proxmox le recupera memoria cuando
    el host pasa del 80 %.
  - Bajarle la RAM fuera de los builds.

---

## Cómo se usa este documento

1. Durante el despliegue: **se agregan hallazgos, no se arreglan**.
2. Al terminar: se revisa la sección **A** con el operador y sale el **plan de cambios de código**
   (qué se cambia, por qué, en qué orden, y qué queda como decisión de diseño). Ese plan se lleva a
   [`PENDING-BUGS.md`](PENDING-BUGS.md), que es donde se sigue el estado de cada tarea.
3. La sección **B** alimenta el `HANDBOOK.md` (recetas) y, donde corresponda, los scripts de infra.
