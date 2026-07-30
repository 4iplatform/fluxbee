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
