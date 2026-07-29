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

### B-6 🟢 `build-deb.sh` podía producir un `.deb` truncado sin fallar

- Con el disco lleno, `dpkg-deb` salía con código 0 pero escribía un paquete de ~1.8 KB sin
  `data.tar`. **Ya corregido** (commit `01db2cc`): preflight de espacio + verificación de integridad
  del `.deb` con fallo ruidoso.

---

## Cómo se usa este documento

1. Durante el despliegue: **se agregan hallazgos, no se arreglan**.
2. Al terminar: se revisa la sección **A** con el operador y sale el **plan de cambios de código**
   (qué se cambia, por qué, en qué orden, y qué queda como decisión de diseño).
3. La sección **B** alimenta el `HANDBOOK.md` (recetas) y, donde corresponda, los scripts de infra.
