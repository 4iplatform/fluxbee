# PENDING-BUGS — tareas de código abiertas por el test integrado en PROD alpha

**Qué es esto.** [FINDINGS.md](FINDINGS.md) es el registro de *hallazgos* — qué se observó y con
qué evidencia. Este documento es la otra mitad: la lista de **tareas de código pendientes** que
salen de esos hallazgos, con el arreglo propuesto y si bloquea o no.

Regla de entrada: acá solo entra lo que **requiere tocar el producto**. Los hallazgos de
infraestructura (serie `B-*` de FINDINGS) no entran salvo que impliquen un cambio en fluxbee.

**Estados:** 🔴 abierto · 🟡 mitigado (anda, pero el arreglo correcto está pendiente) · ✅ cerrado

| id | estado | tema | bloquea |
|---|---|---|---|
| [PB-1](#pb-1) | 🟡 mitigado | SY.edge no recarga el cert TLS al rotar en el vault | no |
| [PB-2](#pb-2) | 🔴 abierto | `vault_put` no valida el material TLS que acepta | no |
| [PB-3](#pb-3) | 🔴 abierto | `tls` no es un `resource_type` de primera clase | no |
| [PB-4](#pb-4) | 🔴 abierto | `sy-edge` crash-loopea en el arranque en frío esperando al vault | no |
| [PB-5](#pb-5) | 🔴 abierto | El orchestrator no configura placas de red secundarias (`A-1`) | no |
| [PB-6](#pb-6) | 🔴 abierto | `harden_ssh` viene en `false` por defecto (`A-2`) | no |
| [PB-7](#pb-7) | 🔴 abierto | Timeout del admin (180 s) vs. las esperas internas de `add_hive` (`A-3`) | no |
| [PB-8](#pb-8) | 🔴 abierto | `egress.gateway_ip` no se propaga a motherbee (`A-4`) | no |

### Serie U — el sistema de update (auditado 2026-07-30, antes de usarlo)

| id | estado | tema | bloquea |
|---|---|---|---|
| [U-1](#u-1) | ✅ **cerrado y VALIDADO en vivo** | El update del core reportaba éxito sin reiniciar nada | — |
| [U-2](#u-2) | ✅ **cerrado** | Ingress y egress no tenían canal de core · resuelto partiendo `dist` por contenido + core por rol | — |
| [U-3](#u-3) | 🔴 abierto | El `.deb` pisa `dist/runtimes/manifest.json` y borra lo publicado en caliente | no (hoy) |
| [U-4](#u-4) | 🔴 abierto | Cero verificación de compatibilidad de versión entre motherbee y spokes | no |
| [U-5](#u-5) | 🔴 abierto | No hay rollback de core como comando | no |
| [U-6](#u-6) | 🟡 mitigado | La carrera era **el orquestador contra sí mismo**: escritura no atómica de 35 MB | — |
| [U-7](#u-7) | ✅ **cerrado** | El watchdog reiniciaba syncthing cada ~7 s por una carpeta inexistente | — |
| [U-8](#u-8) | 🔴 abierto | Operaciones largas sin progreso consultable · **`update category=core` también da TIMEOUT a los 60 s** | no |
| [U-9](#u-9) | 🔴 abierto | Las rutas desconocidas devuelven `{"error":"not_found"}` pelado | no |
| [U-10](#u-10) | ✅ **cerrado y VALIDADO en vivo** | Un upgrade del `.deb` dejaba huérfano a todo nodo runtime | — |
| [U-11](#u-11) | 🔴 abierto | **IO.cloud no puede auto-publicar su canal**: el gate anti-relay le bloquea `externalize` | sí, sin workaround del operador |

---

<a id="pb-1"></a>
## PB-1 🟡 — `SY.edge` no recarga el certificado TLS cuando rota en el vault

**Estado:** mitigado con el patrón de la casa. El arreglo correcto (hot-reload) sigue pendiente
y es una **decisión ya tomada por el operador**: sale así ahora porque no es bloqueante.

### Qué pasa

El material TLS se lee **una sola vez al boot** ([sy_edge.rs:271-290](../../src/bin/sy_edge.rs#L271-L290))
y queda en un `Arc<rustls::ServerConfig>` inmutable ya movido adentro del listener. El handler de
`VAULT_SECRET_CHANGED` existía y funcionaba, pero solo llamaba a `resolve_secrets()`, que refresca
los **secretos de canal** — el TLS no lo tocaba nadie.

No faltaba el broadcast ni el handler: faltaba que el handler hiciera algo cuando la key que
cambió era la del TLS.

### Evidencia en producción (2026-07-30)

Reproducido en vivo con el cert real:

```text
vault_put edge_tls_fluxbee_ai  ->  status: ok, version: 2, changed: true   (cadena de 3 certs)
broadcast VAULT_SECRET_CHANGED emitido
openssl s_client contra el edge  ->  certs servidos: 1                     (la version 1, vieja)
systemctl restart sy-edge
openssl s_client contra el edge  ->  certs servidos: 3                     (la version 2)
```

Peor aún: el síntoma es **silencioso y engañoso**. Un cliente indulgente (navegador, curl con la
CA cacheada) completa el handshake igual buscando el intermedio por AIA, así que
`tls_verify=0` y "anda todo bien" — mientras un cliente estricto (webhooks de Meta/Slack, Go,
Python) falla. Se ve bien justo donde no importa.

### Qué se hizo

Se aplicó el **contrato que ya usa la casa** para material de vault que solo se puede consumir al
arranque: `exit(0)` y que systemd reinicie. Es literalmente lo que hacen `sy.identity` y
`sy.storage` para el secreto de postgres, y está documentado como Model D' VA-J'-13:

> *"The reaction is `exit(0)` so systemd reboots the node and identity re-resolves vault with the
> secret cleanly."*

Detalles del arreglo:

- Reusa el chequeo de origen *fail-closed* que `sy_edge` **ya tenía** (solo el `SY.vault@<vault_hive>`
  que este edge usa puede disparar el restart; si no, cualquier nodo forjando el broadcast lo
  reinicia en loop).
- **Matchea por key**, apartándose a propósito de la guía `(resource_type, tenant_id, ilk)` que
  documenta `VaultSecretChangedPayload`. Esa guía es para consumidores que resuelven *por interés*;
  el edge está configurado con **un `tls_vault_key` explícito** y busca por esa key exacta, así que
  la igualdad de key es justo la condición que invalida lo que cargó.
- **Un `delete` NO reinicia.** Reiniciar sin cert es *fail-closed* → el edge se queda sin frontend
  HTTPS. Convertiría una llamada equivocada al vault en una caída pública. Sigue sirviendo el cert
  que ya tiene y grita por log.
- La decisión se extrajo a `tls_secret_change_action()`, una función pura, para que sea testeable
  (con `process::exit` inline no lo era). 4 tests de regresión.

### Lo que falta (el arreglo correcto)

Hot-reload real: `ResolvesServerCert` de rustls leyendo de un `ArcSwap`, de modo que el broadcast
swapee el cert **sin tocar el socket**. Cero caída. Es el mecanismo idiomático de rustls, no un
invento. El edge es la puerta pública: es el único nodo donde el `exit(0)` de la casa tiene un
costo visible desde afuera.

**Por qué no se hizo ahora:** decisión del operador — tiene que salir andando, y el patrón de la
casa no bloquea. La rotación del cert es anual (Sectigo), así que el reinicio breve se tolera.

---

<a id="pb-2"></a>
## PB-2 🔴 — `vault_put` acepta material TLS sin validarlo

**Cómo apareció.** El `.crt` de cadena completa que entregó el operador venía **mal formado**: el
`-----END CERTIFICATE-----` del leaf y el `-----BEGIN CERTIFICATE-----` del intermedio estaban
**pegados en el mismo renglón**, más CRLF mezclado. Con eso `openssl` lee **cero** certificados del
archivo. (Se normalizó antes de cargarlo; la cadena en sí era correcta.)

**El problema de producto.** `vault_put` con `resource_type=tls` acepta cualquier string. Si ese
archivo roto se cargaba tal cual, el `put` respondía `ok` y el fallo aparecía **mucho después**, al
próximo boot del edge, con un síntoma que no menciona al vault para nada: *"el edge no levanta
HTTPS"*. Y como el edge es *fail-closed*, eso es directamente la puerta pública caída.

**Arreglo propuesto.** Que `vault_put` valide según `resource_type` antes de escribir: para `tls`,
que `value.cert` parsee como una cadena PEM no vacía, que `value.key` parsee como clave privada, y
que **el modulus coincida**. Rechazar con un error claro. Es el mismo espíritu *fail-loud* que ya
tiene el resto del sistema — mover el fallo al momento del `put`, donde el operador lo puede
corregir, en vez de al boot del edge.

Vale la pena mirarlo como un seam general (`validate_secret_value(resource_type, value)`) para que
sirva también a `postgres` (la regla de "credenciales + host, sin dbname" hoy vive solo en la
documentación de la acción, no en un validador).

---

<a id="pb-3"></a>
## PB-3 🔴 — `tls` no es un `resource_type` conocido por el SDK

La ayuda de `vault_put` lista los tipos canónicos (`postgres`, `openai`, `slack`, `bearer_token`, …)
y aclara que los strings desconocidos *"se permiten solo como escape hatch"*. **`tls` no está en la
lista** — pero es el tipo que consume `SY.edge`, un componente del core, para la puerta pública.

Está funcionando como escape hatch. Debería ser de primera clase: es un recurso del propio producto,
no una integración de terceros. Se relaciona con PB-2: sin tipo canónico no hay dónde colgar el
validador.

---

<a id="pb-4"></a>
## PB-4 🔴 — `sy-edge` crash-loopea en el arranque en frío esperando al vault

**Observado** tras el reboot del host del 2026-07-30 (las 4 VMs arrancaron en frío a la vez):

```text
sy-edge.service: Failed with result 'exit-code'.
sy-edge.service: Scheduled restart job, restart counter is at 9.
```

Terminó levantando bien y el mesh reconectó solo — **no hubo intervención manual**. Pero nueve
reinicios para llegar ahí es mucho ruido para un arranque normal, y confunde el diagnóstico: si
alguien mira el journal en ese momento, ve un servicio "fallando".

**A revisar:** si hay backoff, si el edge debería esperar al vault en vez de morir, y si el
`exit(0)` de PB-1 se distingue con claridad de estos fallos en el log y en el contador de restarts
de systemd.

---

<a id="pb-5"></a>
## PB-5 🔴 — El orchestrator no configura placas de red secundarias

Detalle completo y evidencia en [FINDINGS.md → A-1](FINDINGS.md).

Los roles `ingress` y `egress` **requieren** una segunda placa por definición, pero `add_hive` no
tiene forma de declararla: hubo que configurarla a mano en las VMs antes de unirlas. Eso rompe la
promesa de *"corré un comando contra una máquina Linux vacía"* justo en los dos roles que más la
necesitan, y en un prod de verdad (bare-metal, otro hipervisor) no hay una API de Proxmox a la que
recurrir.

---

<a id="pb-6"></a>
## PB-6 🔴 — `harden_ssh` viene en `false` por defecto

Detalle en [FINDINGS.md → A-2](FINDINGS.md).

El endurecimiento funciona muy bien cuando se pide (verificado nodo por nodo en este despliegue),
pero hay que **acordarse** de pedirlo. Un default inseguro en el camino feliz.

---

<a id="pb-7"></a>
## PB-7 🔴 — El timeout del admin puede quedar corto para `add_hive`

Detalle en [FINDINGS.md → A-3](FINDINGS.md).

---

<a id="pb-8"></a>
## PB-8 🔴 — `egress.gateway_ip` no se propaga a motherbee

Detalle en [FINDINGS.md → A-4](FINDINGS.md).

**Sin validar empíricamente todavía.** La lectura del código dice que la ruta por el egress se
declara a los workers pero no a motherbee — y motherbee es justo quien llama a OpenAI, Slack y Meta.
La prueba está pendiente: declarar `egress.gateway_ip: 10.10.10.40` en el hive.yaml de MB y ver por
dónde sale el tráfico.


---
---

# Serie U — el sistema de update

> Auditado el **2026-07-30**, antes de usarlo, con verificación adversarial y comprobación en las
> máquinas de producción. El disparador fue la decisión del operador: *"para resolver los problemas en
> prod sería mejor tener el update andando bien, y después ir por los bugs"*.
>
> **Lo que hay construido funciona más de lo que uno esperaría** — hay manifest con sha256, copia
> atómica con `rename()`, backup de los binarios previos, rollback local ante fallo, gate de
> staleness por hash, y para *runtimes* hasta fanout con `sync_to`/`update_to`. El problema no es que
> falte maquinaria: son **dos defectos puntuales que la vuelven inoperable**, y ambos son silenciosos.

<a id="u-1"></a>
## U-1 ✅ CERRADO — El update del core reportaba éxito **sin reiniciar nada**

La función que corre después de intercambiar los binarios se llama
`restart_local_core_services_with_health_gate`… y llama a **`systemctl start`**:

```rust
// src/bin/sy_orchestrator.rs:3248
systemd_start(&service)?;          // fn systemd_start -> Command::new("systemctl").arg("start")
wait_for_service_active(&service, …).await?;
restarted.push(service);           // <-- y lo reporta como "restarted"
```

`systemctl start` sobre un servicio **que ya está corriendo** es un no-op: systemd devuelve éxito y no
hace nada. El proceso sigue ejecutando el binario viejo (el inode anterior, todavía mapeado). Pero el
nombre se apila en `restarted` y la API responde `status: ok` con la lista completa.

**No existe ningún `systemd_restart` en el archivo.** Las rutas remotas (por SSH, líneas 6361/6512/6678
y 16168) sí usan `systemctl restart` — o sea que **la ruta local es la divergente**, que por la regla
de mirar a los pares es justo la señal de dónde está el bug.

**Por qué es el peor tipo de fallo:** los binarios *sí* quedan cambiados en `/usr/bin`. El update
entonces "funciona"… en el próximo reboot. Alguien puede correr el update, ver `ok`, reiniciar la
máquina por otra razón días después, y concluir que el mecanismo anda bien.

### Se descartó la explicación inocente

Antes de tocar nada verifiqué si el flujo **detenía** los servicios antes de intercambiar los
binarios — en ese caso `start` habría sido correcto. **No los detiene:** los binarios se cambian en
caliente y el propio comentario del código dice la intención,
*"re-render units and daemon-reload BEFORE the restart, so the new binaries start under the new
units"*. Además el camino de rollback vuelve a llamar la misma función esperando que los servicios
levanten con el binario restaurado — que con `start` tampoco ocurriría.

### Arreglo aplicado

`systemd_start` → `systemd_restart` (función nueva, alineada con la ruta remota). `systemd_start`
**se conserva** porque sigue siendo correcto en los 5 lugares donde se arranca algo que no está
corriendo.

### ✅ VALIDADO EN VIVO — `update category=core` contra el worker de producción

Escenario ideal, y se dio solo: el worker corría `sy-orchestrator` del join original (17:11)
mientras syncthing ya le había entregado el binario nuevo en `dist/core/bin`. O sea, exactamente
el estado que U-1 dejaba congelado.

```text
ANTES   pid=4027   desde 17:11:10   /usr/bin/sy-orchestrator = 0716f1f1…
        dist/core/bin/sy-orchestrator = 1e2726ed…            <- delta real

DESPUES pid=54775  desde 22:25:27   exe=/usr/bin/sy-orchestrator   (SIN "(deleted)")
        /usr/bin = 1e2726ed… = dist                          <- promovido
```

**El assert de `/proc/PID/exe` pasa en los tres servicios probados.** Antes del arreglo, el update
habría contestado `ok` con los servicios intactos y `exe` apuntando a un inode borrado.

El worker quedó con 10 servicios corriendo, 0 fallidas, y `connected` en el mesh.

**Lo que el mismo test destapó**: la llamada devolvió `TIMEOUT` a los 60 s aunque la operación
**sí se completó**. Es esperable —el update reinicia al propio `sy-orchestrator` del destino, que
es quien debía responder— pero le da al operador un `error` sobre una operación exitosa. Se suma a
[U-8](#u-8).

**Test que falta** (hoy tiene que fallar, y esa es la prueba):

```bash
p=$(systemctl show -p MainPID --value sy-config-routes)
readlink /proc/$p/exe        # si dice "(deleted)" -> corre el binario viejo
systemctl show -p ActiveEnterTimestamp --value sy-config-routes   # anterior al update
```

---

<a id="u-2"></a>
## U-2 ✅ CERRADO — Ingress y egress no tenían canal de core

El `hive.yaml` que motherbee escribe para esos dos roles trae el canal **apagado a mano**, sin
parámetro:

```
src/bin/sy_orchestrator.rs:18211  (egress)   dist:\n  path: "…"\n  sync:\n    enabled: false
src/bin/sy_orchestrator.rs:18839  (ingress)  dist:\n  path: "…"\n  sync:\n    enabled: false
```

El **worker** lo tiene parametrizado (línea 17440) y en prod está en `true`. **Verificado en las
máquinas de producción:**

```text
ingress1  dist.sync.enabled: false      worker1  dist.sync.enabled: true
```

`POST /hives/ingress1/update {category:"core"}` se queda en `202 sync_pending` para siempre, y el
mensaje no dice que el canal **no existe**.

### La maquinaria ya está ahí — falta compartir la carpeta

**El canal de software de fluxbee es syncthing, no SSH.** SSH existe solo para el bootstrap del
`add_hive`; que se revoque al cerrar el join es el comportamiento correcto y deseado, no un segundo
problema. Verificado en las máquinas de producción:

| carpeta syncthing | worker1 | ingress1 |
|---|---|---|
| `fluxbee-blob` → `blob/active` | `sendreceive` | **ausente** ← correcto, invariante P5 |
| `fluxbee-blob-public` → `blob/public` | — | `receiveonly` |
| **`fluxbee-dist` → `dist/`** | **`receiveonly`** | **AUSENTE** ← *esto* es el bug |
| devices emparejados | 8 | 6 |

O sea: **syncthing ya corre en el ingress, ya está emparejado con motherbee, y ya recibe una carpeta
en receive-only.** No hay que montar infraestructura nueva ni reabrir ningún canal: falta compartirle
`fluxbee-dist`, con exactamente la misma postura `receiveonly` que ya tiene el worker.

**Y no choca con la invariante del DMZ.** `docs/io-blob-spec-v1.md:53` (P5) y la línea 278 son
específicas de **blob**: *"DMZ never receives `active/`… nunca agregar ingress como device del folder
`fluxbee-blob`"*. `fluxbee-dist` es distribución de software, no contenido de blobs; la invariante no
lo alcanza. (Una auditoría automática de este hallazgo confundió las dos carpetas y puso una salvedad
que no aplica — queda anotado para que no se propague.)

### La causa raíz real: `public_only` hace dos trabajos

El `hive.yaml` era la mitad visible. La compuerta de verdad está en `reconcile_syncthing_peer_xml`:

```rust
if !public_only && dist.sync_enabled && dist_sync_tool_is_syncthing(dist) {
    // crea fluxbee-dist + agrega el device
}
```

Para el ingress `public_only = true`, así que **la carpeta se saltea sin importar `sync_enabled`**, y
hay un test que fija ese comportamiento en la punta de motherbee
(`ingress_public_peer_profile_never_attaches_private_folders`).

O sea que la bandera agrupa dos cosas: **(1)** no compartir `blob/active` — invariante P5, correcto —
y **(2)** no compartir `dist/`, que es lo que deja al ingress sin camino de update. La invariante
solo exige la primera.

### Por qué no alcanza con quitar el gate

`dist/` no son solo los binarios del core: contiene `core/` (389 M), **`runtimes/` (64 M)** y
`vendor/` (35 M). `runtimes/` es donde `SY.wf-rules` publica los paquetes de workflow — o sea,
potencialmente contenido de clientes. Compartir `dist/` entero con una caja del DMZ le deja copia de
**todos** los runtimes, incluidos los que ese nodo nunca va a ejecutar.

### Diseño acordado con el operador (2026-07-30): **carpeta de dist por rol**

> *"me parece perfecto que sea carpeta dedicada por role de hive… así se sincroniza solo esa carpeta
> según el role y minimizamos el tema de seguridad"*

Cada rol recibe **solo lo que ese rol ejecuta**. Se apoya en dos cosas que **ya existen**:

1. **`core_component_names_for_role(&manifest, role)`** ya calcula el set de binarios por rol, desde
   `system_nodes.<role>` del `hive.yaml`.
2. **El bootstrap por SSH ya hace exactamente esto:** `sync_core_to_worker` usa ese mismo set en modo
   `worker-bootstrap-minimal`. ⇒ **el scoping por rol ya está implementado en un camino y falta en el
   otro** — otra vez la divergencia entre pares señalando la respuesta.

Y el patrón de carpeta acotada también existe: `fluxbee-blob` (privada, completa) vs
`fluxbee-blob-public` (one-way, acotada a un subdirectorio).

**Forma propuesta:**

```text
MOTHERBEE                                   SPOKE
dist/roles/<role>/core/bin/<set del rol>  ->  dist/core/bin/...     (mismo folder id,
dist/roles/<role>/core/manifest.json      ->  dist/core/manifest.json  path distinto por device)
dist/roles/worker/runtimes/               ->  dist/runtimes/        (solo roles que spawnean)
```

**Lo verificado que hace esto barato:** `local_core_bin_source_path` es fijo y
`validate_core_manifest_for_bins` valida **solo los binarios que se le pasan** (los del rol), no
exige que todo el manifest exista en disco. ⇒ si a cada spoke le llega su carpeta montada en `dist/`,
ve `dist/core/bin/…` como siempre y **el camino de aplicación del update no se toca**.

### Implementado — la forma final, y por qué difiere de la primera idea

Antes de escribir código se mapeó toda la superficie y se atacó el diseño de forma adversarial.
**Tres objeciones resultaron correctas y fatales para el diseño tal como estaba enunciado**, cada
una verificada a mano en el código:

1. **Scopear "solo los binarios del rol" rompía al worker.** No existe `sync_runtime_to_worker`:
   `runtimes/` viaja **únicamente** por syncthing, y es el único camino de entrega de paquetes
   publicados. Un árbol con solo `core/bin` dejaba `publish --sync_to worker` roto para siempre.
2. **Sacar `vendor/` congelaba syncthing en todos lados.** `update category=vendor` saca el hash
   del binario de syncthing de ahí; sin eso, `MANIFEST_INVALID` permanente.
3. **Un manifest recortado por rol rompía el gate de staleness.** El gate compara el sha256 del
   `manifest.json` entre motherbee y el destino; un manifest por rol difiere por construcción, así
   que **todo update quedaría rechazado como stale para siempre.**

**La forma final** parte `dist` por **contenido** (los tres subdirectorios ya existían en disco y son
hermanos, sin anidamiento) y scopea **por rol solo los binarios**:

| folder | motherbee | spoke | quién |
|---|---|---|---|
| `fluxbee-dist-core-<rol>` | `dist/core/<rol>` | `dist/core` | los hives de ese rol |
| `fluxbee-dist-vendor` | `dist/vendor` | `dist/vendor` | todo hive con syncthing |
| `fluxbee-dist-runtimes` | `dist/runtimes` | `dist/runtimes` | **solo worker** |

**El manifest viaja completo**, byte a byte igual al de motherbee, para que el gate de hash siga
funcionando. Es metadata (nombres, versiones, hashes) — no ejecutables — así que la caja del DMZ
sigue teniendo en disco **solo los binarios que corre**: el ingress recibe 4 en vez de 17, sin los
181 MB de `sy_architect`, que es exclusivo de motherbee.

**La asimetría de rutas es lo que evita tocar nada del lado del spoke:** motherbee sirve
`dist/core/<rol>` y el spoke lo recibe como su propio `dist/core`, así que sigue leyendo
`dist/core/bin/…` y `dist/core/manifest.json` con las mismas constantes de siempre.

### Detalles que salieron del análisis y quedaron en el código

- **El folder legado `fluxbee-dist` se REMUEVE, nunca se re-apunta.** `ensure_syncthing_folder_in_config_xml`
  reescribe la ruta de un id que coincide, y un spoke `receiveonly` aplica los borrados remotos —
  el mecanismo exacto del incidente ya registrado en este repo
  (`CORE_DIAG_runtime_deletion_by_fluxbee_syncthing.md`, syncthing borró `runtimes/ai.common/0.1.2/**`).
  Hubo que **escribir la primitiva de borrado de folder, que no existía**: todo el reconcile era aditivo.
- **Carpetas creadas en modo aislado** (`ensure_isolated_...`), para que una carpeta nueva no herede
  la lista de peers de otra y termine compartiéndose con hives que no corresponden.
- **Copias, nunca hardlinks:** `build-deb.sh` e `install.sh` escriben con `install -m0755`, que hace
  unlink+create — un hardlink quedaría apuntando al inode viejo mientras el manifest anuncia el nuevo.
- **Escritura atómica** (temp + rename) del manifest y de cada binario: un lector no puede observar
  un archivo a medio escribir.
- **Poda:** un rol que deja de correr un servicio deja de recibir su binario.
- **Pre-flight en motherbee:** si un rol reclama un componente que el build no trae, falla ahí, fuerte
  y visible, en vez de producir un árbol corto que en el spoke sería un crash-loop.
- **El `sync-hint` lo resuelve el hive destino.** El que pide nombra el *canal*, no el layout;
  motherbee y los scripts e2e siguen mandando `folder_id: "fluxbee-dist"` sin cambios y el destino lo
  expande a las carpetas que su rol realmente tiene, esperando **todas** (un canal parcialmente
  convergido no está convergido).
- **El egress entra a la malla.** Tenía syncthing apagado por rol en dos lugares duros; ahora lo
  decide la config como todos. Fundamento del operador, correcto: syncthing no es un puerto plano —
  el device ID **es** el hash del certificado TLS del peer y solo conectan devices explícitamente
  emparejados.

**Tests:** 8 nuevos, incluida la reescritura del guardián de la invariante del DMZ, que ahora afirma
algo más filoso: el ingress sigue **fuera** de `blob/active` (P5) pero **dentro** de su core y de
vendor, y **fuera** de runtimes. Más uno que prueba que ninguna carpeta de dist se superpone en disco
con otra. **140/140 en verde.**

### Residual conocido

Motherbee materializa el set desde su `system_nodes.<rol>`, y el spoke valida contra la copia que
motherbee le escribió en el join. Coinciden por construcción salvo que se edite el `hive.yaml` de
motherbee **después** del join — que es [U-6](#u-4) (el `hive.yaml` nunca se re-emite a los spokes).
Antes ese drift era inofensivo porque el árbol traía todo; ahora sería un crash-loop al boot del
spoke. Bajo el modelo del operador ese cambio es un **major** (reinstalar desde cero), así que queda
acotado — pero es la razón por la que U-6 sube de prioridad.

---

<a id="u-3"></a>
## U-3 🔴 — El upgrade del `.deb` borra los runtimes publicados en caliente

`dist/runtimes/manifest.json` es un **archivo del paquete** (`dpkg -L fluxbee` lo lista), y el único
`conffile` declarado es `/etc/fluxbee/hive.yaml.example`. Entonces un upgrade **lo pisa**: los
directorios de versión sobreviven, pero desaparecen del manifest → `run_node`/`restart_node` pasan a
`RUNTIME_NOT_AVAILABLE`, y el siguiente `update category=runtime` los recolecta como basura.

Contradice de frente la decisión de packaging: *"Growth = publish + update, no new .deb"*.

**Hoy el daño sería nulo:** los 6 runtimes de prod vinieron del `.deb`, ninguno se publicó en
caliente. Pero apenas `SY.wf-rules` publique un `wf.*` o se despliegue un nodo propio, cada upgrade
los borra. **Es un argumento a favor de probar el update ahora, mientras prod todavía es virgen.**

**Opciones a charlar:** sacar el manifest del paquete · declararlo `conffile` · o hacer *merge* en el
`postinst` en vez de reemplazo.

---

<a id="u-4"></a>
## U-4 🔴 — Cero verificación de compatibilidad entre motherbee y spokes

`GET /hives/{h}/versions` y las alertas de drift son **informativas**: nada rechaza un peer con
versión incompatible. Durante la ventana en que motherbee ya está en `0.1.1` y los spokes siguen en
`0.1.0` no hay ninguna red de seguridad; si un cambio toca protocolo, la malla rompe en silencio.

Mitigación mientras no exista: **mantener el payload de cada update en cambios que no toquen
protocolo**, y no dejar la ventana abierta más de lo necesario.

---

<a id="u-5"></a>
## U-5 🔴 — No hay rollback de core como comando

Existe rollback **automático** (local, ante fallo detectado) y hay backup de los binarios previos en
`state/orchestrator/core-bin.prev.local/update-<ms>/`. Pero **no hay una acción de rollback de core**
— las únicas `*_rollback` son de vault, opa y wf_rules. Y el directorio de backups nunca se limpia.

Peor: el rollback automático depende de **detectar** el fallo, y [U-1](#u-1) hace que muchos fallos no
se detecten.

**Mientras no exista, el rollback real es externo:** snapshot de las VMs + conservar el `.deb` anterior
publicado en el repo apt para poder bajar de versión.


---

<a id="u-6"></a>
## U-6 🔴 — `add_hive` corre una carrera entre el push de vendor por SSH y la carpeta de syncthing

**Encontrado en vivo** al reconstruir prod (2026-07-30), en el primer `add_hive role=ingress`:

```text
status: error
SYNC_SETUP_FAILED  motherbee public Syncthing setup failed:
  vendor manifest size mismatch for syncthing: expected=35806960 actual=22364160
```

**22364160 / 35806960 = 62 %** — un archivo a mitad de escritura.

**Causa, y es consecuencia de [U-2](#u-2):** hasta ahora el ingress no tenía canal de dist, así que
`sync_vendor_to_worker` (push por SSH) era el **único escritor** de `dist/vendor` en el hive que se
unía. Con la carpeta `fluxbee-dist-vendor` agregada, **syncthing escribe el mismo archivo al mismo
tiempo**, y la validación de tamaño —que es fail-closed, y está bien que lo sea— cae sobre el estado
intermedio.

**Verificado que era transitorio:** al inspeccionar después, motherbee y el ingress tenían ambos
`35806960` exacto, y el reintento del join pasó sin tocar nada. Pero **el primer intento falla**, y
eso no es aceptable en un `add_hive`.

**Arreglo propuesto (ordenar, no reintentar):** sembrar vendor por SSH **y recién después** agregar
el device a `fluxbee-dist-vendor`. El orden importa además por una razón de fondo: syncthing necesita
su propio binario para arrancar, así que el push por SSH tiene que venir primero de todos modos —
hoy simplemente no se espera a que termine antes de habilitar la carpeta.

**A charlar:** si además conviene que la validación distinga "todavía convergiendo" de "corrupto",
igual que hace el gate de staleness del update.

---

<a id="u-7"></a>
## U-7 🔴 — El watchdog busca una carpeta que nunca se declara

En motherbee, cada 5 segundos:

```text
WARN failed to verify syncthing folder health; scheduling runtime reconcile
     folder=fluxbee-dist-core-motherbee
     error=syncthing db status ... 404
```

**Bug mío, introducido con [U-2](#u-2).** El watchdog llama
`dist_sync_folders_for_role(dist, state.role, is_motherbee)` con el rol **propio**; en motherbee eso
produce `fluxbee-dist-core-motherbee`. Pero motherbee **no declara esa carpeta** — declara las de
worker/ingress/egress (`ROLE_CORE_DIST_ROLES`), porque no se sincroniza consigo misma.

**Daño real medido antes de alarmarse: ninguno.** `NRestarts=0`; `ensure_blob_sync_runtime` solo
reinicia ante un cambio real de config, así que es un warn ruidoso, no un loop destructivo. Pero
**enmascara problemas reales** — que es exactamente lo que un warn cada 5 s hace.

**Arreglo:** que el watchdog use el mismo criterio que la declaración — si es motherbee, iterar
`ROLE_CORE_DIST_ROLES`; si no, su propio rol.

---

<a id="u-8"></a>
## U-8 🔴 — `add_hive` no tiene forma de consultar su progreso

Un `add_hive` tarda **más de 6 minutos** y no hay manera de preguntarle en qué paso está. El
[command audit log](#) registra el comando **al terminar**, así que durante la ventana no hay nada:
hubo que poleá el proceso de `curl` con `pgrep` para saber si seguía vivo.

Para una operación de esa duración —y que además es la más frágil del sistema, porque toca SSH, red,
systemd y syncthing— es un hueco de operabilidad real. Y empeora con [A-3](FINDINGS.md): el timeout
del admin son 180 s, o sea menos de la mitad de lo que la operación tarda.

---

<a id="u-9"></a>
## U-9 🔴 — Las rutas desconocidas devuelven un `not_found` pelado

`POST /hives/motherbee/hives` (mi error: la ruta correcta es `POST /hives`) devolvió exactamente:

```json
{"error":"not_found"}
```

Sin decir qué ruta, ni qué acción, ni que existe `GET /admin/actions` para averiguarlo. **Contrasta
fuerte con la calidad del resto**: cuando mandé mal el payload del egress, el error fue
*"add_hive role=egress requires an 'egress' object (lan_cidr, wan_iface, lan_iface)"* — perfecto,
accionable. La autodocumentación existe y es excelente; solo falta que el 404 apunte a ella.


---

<a id="u-10"></a>
## U-10 🔴 — Un upgrade del `.deb` deja huérfano a todo nodo runtime · BLOQUEANTE

**Reproducido en producción el 2026-07-30**, en el primer upgrade real (`0.1.1 → 0.1.2`), que era
justamente el camino que este test venía a validar. El `.deb` se instaló limpio
(`Unpacking fluxbee (0.1.2) over (0.1.1)`, 0 unidades fallidas)… y **los cuatro nodos runtime
quedaron en crash-loop**:

```text
fluxbee-node-IO.api-motherbee.service  activating auto-restart
  ExecStart=/var/lib/fluxbee/dist/runtimes/io.api/0.1.1/bin/start.sh
  Main process exited, code=exited, status=203/EXEC
```

### La secuencia exacta

1. Los directorios de runtime están versionados por la versión del paquete. El upgrade
   **instala `0.1.2` y borra `0.1.1`** — en disco solo queda `0.1.2`, y el manifest dice
   `current=0.1.2`.
2. Pero la **unit de systemd** guarda la **ruta absoluta con la versión vieja**, y nadie la
   regenera. `203/EXEC`: el ejecutable ya no existe.
3. **`restart_node` no recupera:** `RUNTIME_NOT_AVAILABLE: version '0.1.1' not available for
   runtime 'io.api'`. La config persistida del nodo también quedó fijada.
4. **`PUT /nodes/{n}/config {"runtime_version":"0.1.2"}` devuelve `ok`** (`config_version: 2`)
   **pero NO rebindea** — lo guarda como una clave de config cualquiera. El `restart_node`
   siguiente falla idéntico. Esto es [U-8 del análisis previo], confirmado: **el `ok` es mentiroso**,
   y eso es peor que rechazar.

⇒ **Por HTTP no hay ninguna forma de recuperar un nodo runtime después de un upgrade.**

### El detalle que lo vuelve más grave

`fluxbee-firstboot` spawnea los nodos con **`"runtime_version":"current"`** — es decir, el operador
pide explícitamente *"seguí al puntero actual"*. El sistema **resuelve `current` una sola vez** y
hornea la versión concreta en la ruta de la unit. No es que el operador haya pinneado: **pidió
seguimiento y recibió un pin silencioso.**

### Recuperación que sí funciona (para el HANDBOOK, mientras no se arregle)

```bash
DELETE /hives/{h}/nodes/{n}            # kill_node — mata el proceso
DELETE /hives/{h}/nodes/{n}/instance   # remove_node_instance — sin esto: NODE_ALREADY_EXISTS
POST   /hives/{h}/nodes                # run_node
  {"node_name":"IO.api","runtime":"io.api",
   "runtime_version":"current",
   "tenant_id":"tnt:00000000-0000-0000-0000-000000000001"}
```

Dos trampas encontradas por el camino: el orden `kill` → `remove_instance` es obligatorio, y
**`tenant_id` es obligatorio** aunque el error no lo diga hasta que falla
(`IDENTITY_REGISTER_FAILED: tenant_id is missing`). **Y esta recuperación pierde la configuración
del nodo**, lo cual en prod (nodos `UNCONFIGURED`) no costó nada, pero en un despliegue con tokens
cargados sería una pérdida real.

### Arreglo aplicado — decisión del operador: **que `current` signifique current**

*"Que siga al puntero"*: al arrancar tras un upgrade, el nodo se rebindea a la versión nueva y
reinicia solo. Es lo que el operador pidió al spawnear, y es lo que hace que un *minor update*
efectivamente llegue.

Se apoya en un patrón que **ya existía para el core** — `regen_local_core_units`, con el comentario
*"Spoke self-heal: re-render them from THIS binary's templates on every boot"*. Estaba aplicado a
los servicios del core y no a los nodos.

**Dos mitades:**

1. **Persistir la intención, no solo su resolución.** `build_node_system_block` ahora guarda
   `requested_runtime_version` junto a `runtime_version`. (El campo ya existía como parcheable en el
   código; lo que faltaba era que `run_node` lo escribiera.) Un config sin el campo —todos los
   anteriores— se lee como `current`, que es correcto porque son justamente los que spawneó
   `fluxbee-firstboot` pidiendo `current`.
2. **Resolver por la intención al boot.** `reconcile_persisted_custom_nodes` resolvía contra
   `node.runtime_version` —la respuesta vieja— y por eso moría con *"version not available"* y
   abandonaba el nodo en pleno crash-loop. Ahora resuelve contra lo pedido: un nodo `current`
   aterriza en la versión nueva y **ese mismo paso de boot lo repara**. Un pin explícito sigue
   pinneado y **falla fuerte** si su versión desapareció, que es lo correcto para un pin deliberado.

Cuando el puntero se mueve se registra en claro (`runtime 'current' pointer moved; rebinding node
from X to Y`) y se persiste la nueva resolución — así el config en disco refleja lo que corre.

**4 tests de regresión**, incluido el del config viejo sin el campo. 147 en verde.

### ✅ VALIDADO EN VIVO — y hicieron falta tres vueltas

Se probó con upgrades reales encadenados en producción. **Cada intento falló distinto, y eso es lo
que permitió llegar al fondo:**

| upgrade | resultado | lo que enseñó |
|---|---|---|
| 0.1.2 → 0.1.3 | ❌ `failed=4` | La re-resolución por intención **sí funcionó** (`pointer moved from=0.1.2 to=0.1.3`), pero el relanzamiento murió: *"Unit … was already loaded or has a fragment file"*. Los nodos son units **transitorias** y systemd no reusa un nombre todavía cargado — y un nodo en crash-loop está en `auto-restart`: no activo, pero cargado, reteniendo su nombre con el `ExecStart` viejo. |
| 0.1.3 → 0.1.4 | ❌ **error idéntico** | Esa identidad era la pista: si la limpieza hubiera corrido y fallado, el mensaje habría cambiado. `systemd_unit_exists()` **agrega `.service` al nombre**, y se le pasaba uno que ya lo tenía → preguntaba por `…service.service` → siempre "not-found" → el guard salía temprano y **no limpiaba nada**. |
| 0.1.4 → 0.1.5 | ✅ **`started=4`** | Los cuatro nodos se repararon **solos, en ~25 s, sin ninguna intervención**. |

La secuencia completa en el log del último:

```text
21:26:10  reconcile completed  started=0  skipped=0  failed=4      <- el upgrade anterior
21:43:28  runtime 'current' pointer moved   from=0.1.4  to=0.1.5
21:43:28  freed stale transient unit name before relaunch
   (×4)
21:43:30  reconcile completed  started=4  skipped=0  failed=0      <- TODOS
```

Units apuntando a `runtimes/<rt>/0.1.5`, configs con `rv=0.1.5 requested=current`.

**Lección de método:** un error que se repite **idéntico** tras un arreglo casi nunca significa "el
arreglo no alcanzó" — significa que **el arreglo no corrió**. Si hubiera corrido y fallado, el
mensaje sería otro.

### Los residuales, resueltos

- ✅ **El rebind por HTTP ya no miente.** La ruta envolvía **todo el cuerpo** bajo `"config"`,
  mientras `set_node_config` lee los cinco campos de binding en el **nivel superior** — así que
  `{"runtime_version":"…"}` aterrizaba como una clave de config inerte y la llamada contestaba
  `ok` sin rebindear. Ahora la ruta los levanta y lo registra.
  **Y además valida**: probándolo en vivo aceptó `9.9.9`, una versión inexistente, con `ok` — el
  mismo pecado con otra cara. Ahora un rebind a una versión no instalada devuelve
  `RUNTIME_NOT_AVAILABLE` en el momento del pedido, no en el próximo restart.
- ✅ **El ruido de SSH se calló.** El reconcile de TLS emitía
  *"failed to distribute mesh TLS material … Permission denied (publickey)"* para cada spoke
  endurecido, en cada pasada. Un hive unido con `ssh_access=revoke` **no tiene** canal SSH a
  propósito: eso es el endurecimiento funcionando, y reportarlo como warn entrena a ignorar el log.
  Ahora, cuando no hay clave de recuperación y el fallo es de autenticación, baja a `debug` con un
  mensaje que dice por qué. **Verificado: 0 warns desde el arranque nuevo.**
- ⚪ **`tenant_id`: descartado.** SÍ está documentado (`sy_admin.rs:10520`, *"AI.* and IO.* managed
  nodes require root-level tenant_id on first spawn"*). Mi error fue no leerlo.
- 🔴 **Queda uno**: el orden `kill` → `remove_instance` es obligatorio y no está dicho en ningún
  lado (`NODE_ALREADY_EXISTS` si se saltea). Va al HANDBOOK.


---

<a id="u-11"></a>
## U-11 🔴 — `IO.cloud` no puede auto-publicar su propio canal

**Encontrado el 2026-07-30** al configurar IO.cloud en producción para conectar Fluxbee Cloud.

Al arrancar con `IO_CLOUD_EDGE_NODE` seteado, el nodo crea su ICH y pide publicarlo. El hive lo
rechaza:

```text
INFO  IO.cloud own ICH ensured  ich_id=ich:14b66389-…  owner=IO.cloud@motherbee  enabled=true
WARN  IO.cloud -> SY.admin externalize REJECTED  error_code=UNAUTHORIZED
      IO.cloud may relay only ["create_tenant","vault_put","run_node"] over the mesh,
      not 'externalize' (Fluxbee Cloud provisioning gate)
```

### Es un choque entre dos controles que por separado están bien

**`authorize_cloud_relay`** (`sy_admin.rs:5401`) es *default-deny* para `IO.cloud@<hive>`: solo
puede relayar las tres acciones de `CLOUD_EXPOSED_ACTIONS`. Eso es FIX-1 y es deliberado — el
comentario explica que existe para que un IO.cloud comprometido no se vuelva *confused deputy*.
Correcto.

**`authorize_channel_command`** (`sy_admin.rs:5374`) es el gate hecho **específicamente** para esto:
solo nodos `IO.*`, y **solo sobre su propia channel** (`owner == caller`). Habría autorizado esta
llamada sin problema: caller `IO.cloud@motherbee`, owner `IO.cloud@motherbee`.

**El problema es que el primero se traga el caso antes de que el segundo pueda opinar.** El
default-deny no distingue dos cosas distintas:

- **relayar** — actuar *en nombre de Cloud*. Es lo que hay que contener, y las tres acciones
  permitidas son exactamente eso.
- **actuar sobre su propia channel** — el nodo actuando *por sí mismo*, sobre un recurso del que
  es dueño. No es un relay, y ya tiene su gate con la restricción de propiedad.

El propio código de IO.cloud dice que esperaba lo segundo
(`nodes/io/io-cloud/src/main.rs:118-123`): *"the same node→admin ADMIN_COMMAND path a real deploy
uses; it is self-service (requester owns the ICH). SY.admin authorizes this by router-stamped IO.*
origin plus `requester == ICH owner`"*.

### Impacto

`IO.cloud` **no es alcanzable desde internet** hasta que un operador publique el canal a mano. Y
hay que repetirlo en cada reinstalación del hive. Documentado como paso obligatorio en
[`docs/io-cloud-api.md`](../../docs/io-cloud-api.md) §5.

**Workaround verificado en producción** (`POST /channels/externalize` desde el admin, donde
`caller=None` es camino interno confiable):

```json
{"ich":"<el del log>","edge_node":"SY.edge@ingress1","inbound_family":"user",
 "auth_mode":"shared-secret","secret":"<IO_CLOUD_SECRET>","methods":["POST"]}
```

### Arreglo propuesto — a charlar

Que `authorize_cloud_relay` **delegue los comandos de channel** a `authorize_channel_command` en vez
de negarlos de plano. Mantiene intacto el default-deny para el relay (que es lo que contiene a un
IO.cloud comprometido) y devuelve la decisión sobre canales propios al gate que ya la sabe tomar,
con su restricción `owner == caller`.

**No lo hago sin tu visto bueno**: tocar un gate de seguridad marcado como FIX-1 (HIGH) merece
acuerdo explícito, aunque la separación conceptual parezca clara.
