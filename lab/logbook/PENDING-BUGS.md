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
| [U-1](#u-1) | 🔴 abierto | **El update del core reporta éxito sin reiniciar nada** | **SÍ** |
| [U-2](#u-2) | 🔴 abierto | **Ingress y egress no tienen canal de core** (`enabled: false` hardcodeado) | **SÍ** para esos roles |
| [U-3](#u-3) | 🔴 abierto | El `.deb` pisa `dist/runtimes/manifest.json` y borra lo publicado en caliente | no (hoy) |
| [U-4](#u-4) | 🔴 abierto | Cero verificación de compatibilidad de versión entre motherbee y spokes | no |
| [U-5](#u-5) | 🔴 abierto | No hay rollback de core como comando | no |

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
## U-1 🔴 — El update del core reporta éxito **sin reiniciar nada** · BLOQUEANTE

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

**Arreglo:** `systemd_start` → un `systemd_restart` nuevo, alineando con la ruta remota. Es una línea.
**A charlar antes:** `restart` vs `stop`+`start` cambia la ventana de indisponibilidad del spoke.

**Test que falta** (hoy tiene que fallar, y esa es la prueba):

```bash
p=$(systemctl show -p MainPID --value sy-config-routes)
readlink /proc/$p/exe        # si dice "(deleted)" -> corre el binario viejo
systemctl show -p ActiveEnterTimestamp --value sy-config-routes   # anterior al update
```

---

<a id="u-2"></a>
## U-2 🔴 — Ingress y egress no tienen canal de core · BLOQUEANTE para esos roles

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

**Arreglo:** parametrizar `dist.sync.enabled` en los `format!` de ingress y egress igual que ya está
en el de worker (línea 17440), y agregar la carpeta al peering de esos roles en `add_hive`.

**A charlar igual:** si el default para ingress/egress debe ser `true`, o quedar explícito en el
`add_hive`. Y para las cajas **ya unidas** (las de prod ahora) hace falta decidir cómo se les agrega
la carpeta sin re-hacer el join, dado que SSH está revocado por diseño.

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
