# PENDING-BUGS — tareas de código abiertas por el test integrado en PROD alpha

**Qué es esto.** [FINDINGS.md](FINDINGS.md) es el registro de *hallazgos* — qué se observó y con
qué evidencia. Este documento es la otra mitad: la lista de **tareas de código pendientes** que
salen de esos hallazgos, con el arreglo propuesto y si bloquea o no.

Regla de entrada: acá solo entra lo que **requiere tocar el producto**. Los hallazgos de
infraestructura (serie `B-*` de FINDINGS) no entran salvo que impliquen un cambio en fluxbee.

**Estados:** 🔴 abierto · 🟡 mitigado (anda, pero el arreglo correcto está pendiente) · ✅ cerrado

> **⚠️ Auditoría adversarial del 2026-08-03**
>
> Los 12 hallazgos abiertos se sometieron a un intento sistemático de **refutación** (regla 0d de
> [METHOD.md](METHOD.md)). Resultado: **solo 2 sobrevivieron intactos** (U-3, U-9); los otros 10
> tenían algo cierto adentro pero el **encuadre, la causa o el arreglo mal** — y **tres de los
> arreglos propuestos eran peligrosos de implementar** (PB-6, PB-8, U-4).
>
> Patrón: *en 6 de 12, el bug real estaba en el mismo archivo, a pocas líneas — y no se vio porque
> ya estaba la hipótesis escrita.* Las entradas de abajo son las **reescritas**; cada una dice qué
> se borró y por qué.

| id | estado | tema | bloquea |
|---|---|---|---|
| [PB-1](#pb-1) | 🟡 mitigado | SY.edge no recarga el cert TLS al rotar en el vault | no |
| [PB-2](#pb-2) | 🟡 reescrito | Nadie verifica la **completitud** de la cadena TLS (el validador propuesto no la atajaba) | no |
| [PB-3](#pb-3) | 🟢 cosmético | El enum `ResourceType` derivó · **sin impacto funcional** | no |
| [PB-4](#pb-4) | ✅ **CERRADO** | El arranque en frío **se veía** como falla: el edge culpaba al vault sin motivo | — |
| [PB-5](#pb-5) | 🟡 reescrito | El egress reporta `nat_applied: true` sin verificar su pata LAN | no |
| [PB-6](#pb-6) | 🟡 reescrito | `ssh_password` no se cierra solo · **⛔ invertir el default deja cajas sin acceso** | no |
| [PB-6b](#pb-6b) | ✅ **CERRADO** | `harden_ssh_applied` era un eco del request: reportaba `false` con el password auth ya apagado | — |
| [PB-7](#pb-7) | 🔴 abierto | `add_hive` (>6 min) desborda la ventana del admin (180 s) **y bloquea el canal** | no |
| [PB-8](#pb-8) | 🟡 reescrito | El egress no tiene historia para motherbee · **⛔ propagarle la ruta = lockout** | no |
| [PB-9](#pb-9) | 🔴 abierto | Todo restart de `sy-vault` recicla un edge sano → bache en el HTTPS público | no |

### Serie U — el sistema de update (auditado 2026-07-30, antes de usarlo)

| id | estado | tema | bloquea |
|---|---|---|---|
| [U-1](#u-1) | ✅ **cerrado y VALIDADO en vivo** | El update del core reportaba éxito sin reiniciar nada | — |
| [U-2](#u-2) | ✅ **cerrado** | Ingress y egress no tenían canal de core · resuelto partiendo `dist` por contenido + core por rol | — |
| [U-3](#u-3) | 🔴 **sobrevive** | El `.deb` pisa `dist/runtimes/manifest.json` · **el arreglo de U-10 NO lo cubre** · toca packaging | **sí**, apenas exista un runtime publicado en caliente |
| [U-4a](#u-4a) | 🟢 reescrito | El HELLO lleva `protocol` y nadie lo mira · **⛔ rechazar el peer = self-fencing** | no |
| [U-4b](#u-4b) | 🔴 abierto | **Las drift-alerts no se escriben nunca**: `GET /drift-alerts` devuelve un snapshot disfrazado de historial | no |
| [U-5](#u-5) | 🟡 gap | No hay rollback de core como comando · **⛔ "restaurar el último backup" repetiría U-1** | no |
| [U-5b](#u-5b) | ✅ **CERRADO** | Los backups de core se acumulaban sin GC **y se creaban vacíos en cada no-op** | — |
| [U-6](#u-6) | 🟡 mitigado | La carrera era **el orquestador contra sí mismo**: escritura no atómica de 35 MB | — |
| [U-7](#u-7) | ✅ **cerrado** | El watchdog reiniciaba syncthing cada ~7 s por una carpeta inexistente | — |
| [U-8a](#u-8a) | 🟡 abierto | La API de admin no tiene contrato para operaciones largas · se cierra con [PB-7](#pb-7) | no |
| [U-8b](#u-8b) | 🟡 reescrito | `update category=core` da TIMEOUT a los 60 s · **la causa que estaba escrita era falsa** | no |
| [U-8c](#u-8c) | ✅ **CERRADO** | El `timeout_unknown` del architect estaba **muerto** por un substring imposible | — |
| [U-9](#u-9) | ✅ **CERRADO** | El catch-all rompía el envelope **y daba 404 donde correspondía 405** | — |
| [U-10](#u-10) | ✅ **cerrado y VALIDADO en vivo** | Un upgrade del `.deb` dejaba huérfano a todo nodo runtime | — |
| [U-11](#u-11) | ✅ **cerrado y VALIDADO en vivo** | IO.cloud no podía auto-publicar su canal | — |
| [U-12](#u-12) | 🟡 corregido | La carrera existe, pero **hay una llamada atómica que la evita** y yo no la encontré | no |

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
## PB-2 🟡 — Nadie verifica la **completitud** de la cadena TLS: una cadena truncada entra al vault y el edge la sirve sin quejarse

> **Reescrito el 2026-08-03 tras auditoría adversarial.** El hallazgo original (*"`vault_put` acepta
> material TLS sin validarlo"*) proponía un validador que **no habría atajado el archivo roto del
> operador**. Se verificó ejecutando el caso real.

**Cómo apareció.** El `.crt` de cadena completa que entregó el operador venía **mal formado**: el
`END CERTIFICATE` del leaf y el `BEGIN CERTIFICATE` del intermedio estaban **pegados en el mismo
renglón**, más CRLF mezclado. `openssl` lee **cero** certificados de ese archivo.

**Lo que es cierto.** `vault_put` no inspecciona `value` (`sy_vault.rs:752-798`).

**Lo que era falso y se borró:**

- *"El material TLS no se valida."* Sí se valida, al resolver: cadena no vacía, key parseable y
  coincidencia key/cert por SPKI, vía `CertifiedKey::from_der` (`sy_edge.rs:1642-1652`). La familia
  entera valida **al resolver, no al escribir** — es el contrato deliberado del vault (*"The vault
  does not interpret it"*, `docs/sy-vault-spec.md:177`).
- *"El fallo aparece mucho después."* Post-PB-1 el `put` dispara `exit(0)` y el material se valida a
  los segundos (`sy_edge.rs:461-475`).
- *"El síntoma no menciona al vault."* `sy_edge.rs:276/288` lo nombran.
- *"A `postgres` también le falta validador."* Existe (`sy_storage.rs:2463-2467`).

**Por qué el arreglo propuesto no servía — probado, no razonado.** Con `END`/`BEGIN` pegados,
`rustls_pemfile::certs` devuelve **`Ok` con 1 certificado y sin error** (openssl: 0). O sea "cadena
no vacía + key parsea + modulus coincide" da **OK sobre el archivo roto**, y el edge levanta HTTPS
sirviendo **solo el leaf**. No es la puerta caída: es una falla **silenciosa** que rompe a los
clientes estrictos.

**El arreglo que sí vale:**

- **(a)** En `sy_edge.rs::tls_config_from_pem`: contar los marcadores `BEGIN CERTIFICATE` del PEM y
  compararlos con los certificados que devolvió el parser. Si no coinciden → fallar fuerte (el PEM
  se truncó en silencio). Si la cadena tiene un solo cert → WARN explícito *"leaf sin intermedios:
  los clientes estrictos van a fallar"*. Ataja el caso real, y vale tanto para el vault como para el
  disco.
- **(b)** *Opcional, decisión del operador* — el mismo chequeo como pre-flight del `put`, o mejor un
  seam en el SDK que llamen los dos para que no puedan divergir. **Rompe la opacidad declarada del
  vault**, por eso no se hace sin OK.
- **(c)** Anotar la **asimetría fail-closed**: un `delete` está deliberadamente protegido de tirar la
  puerta pública (`sy_edge.rs:476-481`), pero un `put` inválido **sí la tira**, y post-PB-1 de
  inmediato: `exit(0)` → boot → `Err` en main → crash loop. O el boot degrada como el `delete`
  (conservar el último material bueno), o se documenta que la recuperación es `vault_rollback`.

---

<a id="pb-3"></a>
## PB-3 🟢 — El enum `ResourceType` derivó respecto de lo que el propio producto escribe

> **Degradado de 🔴 a cosmético/documental el 2026-08-03.** No tiene impacto funcional.

**Hecho.** Al menos cuatro tipos que produce o consume el propio producto no están en `ResourceType`
(`crates/fluxbee_sdk/src/vault.rs:40-87`) ni en la ayuda de `vault_put`: `tls`, `ssh`
(`sy_orchestrator.rs:20649`), `whatsapp` (`io-wapp/src/main.rs:62`) y `linkedhelper_adapter`. La
ayuda los llama *"escape hatch"* mientras la casa los usa como camino normal.

**Lo que NO es.** No hay impacto funcional: `known_wire_values()` **no tiene llamadores**, el vault
filtra por igualdad de string normalizado (`sy_vault.rs:1102-1110`), y un `Custom` se comporta
idéntico a una variante nombrada. En particular **`tls` no lo consume nadie**: SY.edge se enlaza por
`tls_vault_key` y matchea **por key**, no por `resource_type` — la decisión deliberada que ya
documenta PB-1.

**Se borró el link a PB-2:** el validador de PB-2 cuelga del string normalizado y se puede
implementar hoy con `Custom("tls")`. Que `tls` no esté en el enum **no bloquea nada**.

**Arreglo (elegir uno).** (a) Promover las cuatro a variantes y que `known_wire_values()` deje de ser
código muerto — p.ej. WARN cuando admin cae en `Custom`; o (b) reescribir la ayuda para que "custom
normalizado" sea un camino de primera clase y documentar la *value-shape* de cada tipo (a `tls` le
falta la fila `{cert, key}`).

---

<a id="pb-4"></a>
## PB-4 🟡 — El arranque en frío del ingress **se ve** como una falla: el edge dice "arreglá el vault" cuando no hay nada que arreglar

> **Reescrito el 2026-08-03.** Se le sacó el título causal: lo que el hallazgo pedía revisar **ya
> existe y es deliberado**.

**Observado** tras el reboot del host del 2026-07-30: `restart counter is at 9`, y terminó levantando
solo, sin intervención.

**Lo que NO es.** No falta esperar al vault: la espera **es** el reintento. `Restart=always` +
`RestartSec=5` se eligió porque el hive ingress no tiene `sy-vault` local y systemd no puede ordenar
una dependencia cross-hive (`docs/edge-ingress-spec-v6.md:381-386`). Tampoco falta backoff:
`RestartSec=5` es justo lo que mantiene la unit **bajo el rate-limit** de systemd (5 arranques/10 s),
por eso 9 reinicios se curan solos y nunca latchean en `failed`. Ventana total ~45 s.

**El defecto real, chico y concreto.** `sy_edge.rs:304-307` imprime la misma frase — *"Fix the vault
secret / cert files and restart."* — tanto cuando el vault todavía no es alcanzable (transitorio, se
cura solo) como cuando el cert está roto de verdad (hace falta el operador). En el arranque en frío
esa instrucción **es falsa**, y es la única fuente del ruido.

**✅ Arreglado (2026-08-03).** `fetch_tls_config_from_vault` ya no aplana el error a string en el punto
donde todavía conoce el tipo: devuelve `TlsFetchFailure`, que parte el fallo en **`VaultUnavailable`**
(`VaultError::Node` / `ActionTimeout` — no llegó respuesta, systemd reintenta) y
**`MaterialInvalid`** (el vault contestó, o el PEM no sirve — hace falta un humano). El arranque loguea
`warn!` con *"vault no alcanzable todavía; systemd reintenta en ~5 s (normal en arranque en frío)"* en
el primer caso, y el `error!` con la instrucción al operador solo en el segundo. El mensaje
fail-closed final se bifurca igual. 23/23 tests de `sy_edge` verdes.

**No hecho, opcional:** reintentar in-process ~15 s antes del primer `exit`, copiando el patrón de
`sy_identity.rs:6626-6651` — bajaría 9 reinicios a 1-2. Es cosmética sobre un camino que ya se cura
solo; no la hice para no tocar el contrato de arranque sin necesidad.

**Lo que el hallazgo no vio (y es peor).** Después de 98adab7 el contador de restarts **mezcla
`exit(0)` con `exit(1)`**. `sy_vault` emite `VAULT_SECRET_CHANGED op=put` por **cada** secreto en su
propio arranque (`sy_vault.rs:246-260`) y `tls_secret_change_action` no mira `action="bootstrap"`
(`sy_edge.rs:1690-1715`): **todo restart de `sy-vault` reinicia un edge sano** → bache de ~5 s en el
HTTPS público. → ver **PB-9**.

**Rigor.** La causa *"esperando al vault"* se **infirió del código**, no de una línea de journal del
edge, y se propagó como hecho a `HANDBOOK.md:515`. La inferencia se sostiene, pero la próxima vez hay
que capturar el `journalctl -u sy-edge` del arranque en frío antes de escribirla como hecho.

---

<a id="pb-5"></a>
## PB-5 🟡 — El egress reporta `nat_applied: true` sin verificar su pata LAN

> **Reescrito el 2026-08-03.** El título anterior (*"el orchestrator no configura placas
> secundarias"*) pedía algo que es **contra-contrato explícito**.

**Alcance corregido: solo `role=egress`.** Ingress sale del hallazgo — `add_hive` no le pide ni le
puede pedir nombres de interfaz (`IngressSection:210-221`); sus dos placas son una decisión de
topología del spec v6, no un hueco. *(Corregir también `HANDBOOK.md:403-408`, que hoy dice "pasarle
los nombres de interfaz" para ingress y es falso.)*

**Se sacó el encuadre "rompe la promesa de la máquina vacía":** el contrato del *clean box*
(`07-operaciones.md:435-447`) ya pone la red del lado del operador **para todos los roles**, y el
producto tampoco direcciona la placa primaria de nadie. No hay divergencia entre hermanos: hay un
prerequisito uniforme.

**No se pide "que el orchestrator configure las placas".** Es contra-contrato
(`edge-egress-nat-spec.md:378` decide **no** escribir netplan/NM) y desde adentro del guest no se
puede crear una vNIC. Si el operador igual lo quiere, es una **feature nueva a discutir**.

**El bug real (fail-loud, chico, estilo de la casa).** `reconcile_egress_nat` debe hacer preflight
antes de dar por bueno el NAT: que `lan_iface` y `wan_iface` **existan**, y que `cfg.edge_ip` esté
efectivamente asignada a `lan_iface`. Si no → error fatal en el bootstrap con mensaje accionable, y
WARN persistente en cada drift tick, igual que se hace con `internet_reachable`. Agregar `lan_leg_ok`
a `EgressVerification` y **dejar de emitir `egress_nat_applied: true` sin él** — hoy el comentario en
`:19194-19199` declara ese campo *"trustworthy"* y no lo es.

**Por qué el fallo es silencioso (dato técnico que faltaba):** `iifname`/`oifname` matchean por
nombre **en runtime**, así que la ausencia de la placa **no** hace fallar el `nft -f` — a diferencia
de `iif`/`oif`.

---

<a id="pb-6"></a>
## PB-6 🟡 — El canal de bootstrap por `ssh_password` no se cierra solo — y `harden_ssh` a secas **no** es la respuesta

> **Reescrito el 2026-08-03.** La remediación original (*"invertir el default"*) **dejaría cajas sin
> ninguna vía de acceso**. Duplicado parcial de SO-2026-06-30-02.

**Lo que es cierto.** `resolve_add_hive_harden_ssh` default `false` (`sy_orchestrator.rs:20113-20118`),
sin fallback por env, igual en los tres flujos (worker/egress/ingress) — o sea **contrato de familia,
no divergencia**. Con canal `ssh_password`, al terminar el join la caja conserva
`PasswordAuthentication yes`.

**La evidencia original estaba mal.** El hardening tiene **dos** callers:
`apply_add_hive_ssh_controls_after_finalize` (gated por `harden_ssh`, `:20820`) y
`finalize_spoke_key_persist` (**incondicional**, rama `ssh_access=key_only_persist`, `:20711`).

**Impacto reencuadrado.** `add_hive` **nunca enciende** password auth; el canal previo al join es
prerequisito del operador. En el camino recomendado (`ssh_key` sobre imagen cloud) el server ya viene
con password OFF y el default `false` no deja nada abierto. El agujero solo aparece cuando el
operador abrió password a mano — el caso que la doc ya cubre pasando `harden_ssh:true`.

**Se retiró "invertir el default".** `harden_ssh=true` + el default `ssh_access=revoke` = caja **sin
password, sin key de MB y sin sudoers** → recuperable solo por consola del hipervisor. Choca de
frente con el invariante que el propio código declara: *"verify-before-revoke that never strands a
spoke"* (`:20121-20132`).

**Dos opciones sanas, a decidir por el operador:**

- **(a)** Que `ssh_password` implique `ssh_access=key_only_persist` — hardena **y** deja llave
  per-spoke verificada en el vault. Cierra el agujero **sin riesgo de lockout**, y no inventa nada:
  es maquinaria que ya existe.
- **(b)** Devolver `warnings: ["bootstrapped via ssh_password and PasswordAuthentication left
  enabled"]`. El estado **ya es consultable**; lo que falta es el warning.

**Corregir el HANDBOOK:** el slogan *"Entra abierta, sale cerrada"* (`:524`) y la trampa 10 (`:532`)
**inducen el error** — en combinación con el revoke por default es justo lo que dejó las cajas del
despliegue del 2026-07-29 sin ninguna vía SSH.

<a id="pb-6b"></a>
### PB-6b ✅ **CERRADO** — `harden_ssh_applied` era un eco del request, no un hecho medido

Defecto real **en la dirección opuesta** al hallazgo, encontrado al refutarlo. Los tres flujos
(`worker`/`egress`/`ingress`) devolvían literalmente `"harden_ssh_applied": harden_ssh` — el flag
pedido, no lo ocurrido. Con `ssh_access=key_only_persist`, que **hardena incondicionalmente**, un join
con `harden_ssh:false` dejaba la caja **con password auth apagado y verificado** mientras le informaba
al operador que seguía encendido. Un dato de seguridad, mal.

**Arreglado:** `harden_ssh_applied` es ahora un campo medido que viaja en
`AddHiveSshControlsResult` / `SpokeKeyPersistResult` desde donde el trabajo efectivamente ocurre — en
la rama gated, tras propagar con `?` el `disable` + `verify`; en la rama persist, `true` en **los dos**
caminos de retorno, porque el harden ya sucedió antes del punto de decisión del verify-before-revoke.
*(El camino `socket_only` sigue reportando `false`, que ahí es lo correcto: no toca SSH.)*

**Falta la comprobación en vivo:** confirmarlo de punta a punta exige un `add_hive` nuevo. Compila y
el cambio es local al reporte, pero **todavía no lo vi contra una caja real.**

---

<a id="pb-7"></a>
## PB-7 🔴 — El `add_hive` real dura más que la ventana de respuesta del admin (180 s) — y en ese rato el plano de control del hive queda **bloqueado**

**Qué pasa.** `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` default 180 s (`sy_admin.rs:13648-13653`) contra un
`add_hive` **medido en >6 min**. El cliente recibe `{"error_code":"TIMEOUT"}` sobre una operación que
sigue corriendo y suele terminar bien.

**Dónde se va el tiempo (corregido).** **No** en los gates: topean en ~150 s en cualquier camino de
falla, porque el gate de LSA solo corre si el WAN salió bien. El costo dominante es la **fase SSH
previa**: `sync_core_to_worker` + `sync_vendor_to_worker` (~35 MB) + decenas de `ssh` con
`ConnectTimeout=10` pero **sin timeout total** y con reintentos. **Esa fase no tiene techo.**

**Lo que NO es.** No es divergencia ni olvido: el 180 es deliberado, es la ventana **más larga** de la
familia (update 60 / sync_hint 45 / genérico 30). Es un contrato que la realidad desbordó.

**La mitigación que estaba escrita es falsa a medias.** *"Queda pending y el reintento es
idempotente"* vale **solo** si el corte cae después de `write_hive_info` (`:18362`). Durante la fase
SSH **no hay `info.yaml`**: el reintento **borra el directorio y re-bootstrapea de cero**.

**Y "reintentá para ver el estado" es imposible durante el join.** Todo `ADMIN_COMMAND` va a un
**único canal serial** que `add_hive` ocupa entero, además **bloqueando el thread con
`std::thread::sleep` en código async** (`:21726`). `get_hive`/`list_hives`/`hive_status` se encolan,
vencen a los 30 s del lado del cliente, y **después se ejecutan tarde**.

**Opciones (ninguna es "subir el default a 240"):**

1. Subir el default es **placebo**: para cubrir el caso medido harían falta ~600 s de ventana HTTP.
2. **Lo correcto:** partir `add_hive` en **aceptación + trabajo en background** — responder `202` con
   el hive en estado `joining`, correr el flujo en una tarea propia (lo que además saca el
   `std::thread::sleep` del executor) y **dejar el canal admin libre**. Cierra PB-7 y U-8a juntos:
   son el mismo hueco visto por dos puntas.
3. **Barato y honesto mientras tanto:** escribir `info.yaml` con `status: joining` **al principio**
   del flujo, y que el TIMEOUT devuelva un cuerpo que lo diga. Sin (2) igual no se puede consultar
   hasta que el worker se libere — decirlo explícito.
4. Documentar en el HANDBOOK que **un TIMEOUT de `add_hive` no significa fracaso**.

---

<a id="pb-8"></a>
## PB-8 🟡 — El modelo de egress **no tiene historia para motherbee** — y motherbee es donde corren los nodos que llaman a internet

> **Reescrito el 2026-08-03.** El arreglo que sugería el título anterior (*"propagar `gateway_ip` a
> MB"*) **es un lockout del plano de control**.

**Qué es (no lo que decía).** `egress.gateway_ip` en el hive.yaml de MB significa, por contrato
explícito y documentado, *"por dónde salen los **workers**"* (`sy_orchestrator.rs:181-184`). No es un
valor que "no se propaga": **nunca fue para MB**. La exclusión es un brazo explícito junto a Ingress
(`:5604`), no un olvido.

**El hueco real.** `base-nodes.json` bootea `IO.slack.default@motherbee`, `IO.wapp.default@motherbee`
y `AI.chat@motherbee`. O sea: el único hive que el modelo de egress **no** rutea es justo el que
hostea a todos los que llaman a OpenAI/Slack/Meta. *(Y `handbook:156` dice "internal hives can reach
the internet through it" cuando el código solo cubre workers → corregir a "workers".)*

**Consecuencia correcta** (reemplaza a *"saldría por la red de admin"*): MB sale hoy por el SNAT del
hipervisor, **una excepción temporal marcada para cerrarse**. Cuando se cierre, MB pierde internet y
no hay mecanismo gestionado para devolvérsela. La red de admin está en **los dos** caminos — la pata
WAN del propio egress es `192.168.8.240` — así que el problema no es "sale por admin", es **"sale por
fuera del chokepoint gestionado"**, donde el spec §11 reserva allow-list y logging.

**⛔ Arreglo prohibido:** agregar `HiveRole::Motherbee` al brazo `Worker` de `reconcile_egress`.
Pondría al **plano de control** a reescribirse su default route en cada boot y cada tick de watchdog,
desde un campo de yaml, sin rollback, en la caja que provisiona el egress. Para un worker el fallo es
no-fatal **por diseño**; en MB es **lockout**.

**Opciones (ninguna se implementa sin OK):** (a) la ruta de MB la configura el operador a nivel
SO/netplan y Fluxbee solo la **verifica** y grita si no coincide (read-only, sin riesgo); (b) campo
separado y explícito `egress.motherbee_route: true` (opt-in, **nunca** por herencia del campo de
workers), con gate de alcanzabilidad y sin re-aplicación en watchdog; (c) mover los nodos que llaman
a internet a un worker — la única opción que ya funciona hoy.

**La trampa más inmediata, que faltaba:** hoy **no rutea nadie** por el egress — MB aún no declara la
sección y worker1 se unió antes, y la inyección solo alcanza a workers provisionados **después**.
Y `packaging/hive.yaml.example` **ni siquiera tiene la sección `egress:`** de nivel raíz, así que
declararla es una edición a mano no documentada.

---

<a id="pb-9"></a>
## PB-9 🔴 — Todo restart de `sy-vault` reinicia un `SY.edge` **sano** → bache en el HTTPS público

> Descubierto por la auditoría adversarial del 2026-08-03, mientras se refutaba PB-4. No estaba en
> ningún hallazgo.

`sy_vault` emite `VAULT_SECRET_CHANGED op=put` por **cada secreto que tiene** durante su propio
arranque (`sy_vault.rs:246-260`, `530-596`) — es su bootstrap, no un cambio real. Y
`tls_secret_change_action` (`sy_edge.rs:1690-1715`) **no mira `action="bootstrap"`**: si la key
coincide con `tls_vault_key`, dispara el `exit(0)` de recarga.

**Consecuencia:** cada vez que `sy-vault` reinicia (upgrade del `.deb`, reboot, restart manual), el
edge — que estaba perfecto — se recicla. En el HTTPS público eso es un bache de ~5 s.

**Arreglo:** que `tls_secret_change_action` devuelva `NoAction` cuando el evento viene marcado como
bootstrap, o que `sy_vault` no emita `op=put` por secretos preexistentes en su arranque. Lo primero
es más chico y local; lo segundo arregla también a cualquier otro suscriptor. Hay que mirar quién más
reacciona a `VAULT_SECRET_CHANGED` (io.slack lo hace) antes de elegir.


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
## U-3 🔴 — El upgrade del `.deb` borra los runtimes publicados en caliente · **SOBREVIVE (y es peor)**

> **Sobrevivió la auditoría intacto en su núcleo**, pero estaba **mal calibrado en tres cosas y
> subestimaba el daño en dos.** Reescrito el 2026-08-03.

`dist/runtimes/manifest.json` es un **archivo del paquete** (`dpkg -L fluxbee` lo lista **desde
23c4175 / 2026-07-22**; los `.deb` anteriores no traen `dist/runtimes` en absoluto), y el único
`conffile` declarado es `/etc/fluxbee/hive.yaml.example`. Un upgrade **lo pisa**.

**Corrección 1 — el GC no espera a nadie.** El texto decía *"el siguiente `update category=runtime`
los recolecta"*. **No hace falta ninguna acción del operador:** la retención corre sola en el watchdog
cada 300 s (`RUNTIME_VERIFY_INTERVAL_SECS`), y como el `postinst` reinicia `sy-orchestrator`, la
pasada ocurre **a los pocos minutos del upgrade**.

**Corrección 2 — el borrado se parte en dos casos.** La retención también conserva lo que referencian
los configs persistidos (`persisted_runtime_keep_versions_with_root`):

- **con instancia persistida** → el directorio sobrevive al GC y el proceso sigue corriendo, pero
  queda **inarrancable**: cualquier `restart_node`/`run_node`/reboot da `RUNTIME_NOT_AVAILABLE`;
- **sin instancia** (paquete de workflow recién publicado, runtime precargado) → **se borra el
  directorio entero**.

En los dos casos el manifest miente.

**Corrección 3 — y esta es la grave: el arreglo de [U-10](#u-10) NO cubre esto.**
`reconcile_persisted_custom_nodes` resuelve primero la **clave** del runtime contra el manifest y
recién después la versión. Con la clave borrada, el *"que `current` signifique current"* **nunca
llega a correr** y el nodo queda abandonado. O sea: tras el upgrade, **todo nodo montado sobre un
runtime publicado en caliente queda huérfano igual que en U-10, y sin self-heal**.

**Eso lo sube de "no bloquea (hoy)" a "bloquea el día que exista el primer runtime publicado en
caliente" — y ese día ya llegó en el lab (`wf.router 0.0.4` en la hive 240).**

**Corrección 4 — el merge NO hay que construirlo, ya existe.** `scripts/publish-runtime.sh` mergea
(lee el manifest previo) y el instalador hermano ya lo usa así contra el root vivo
(`install.sh:895-960`, `--dist-root /var/lib/fluxbee/dist`). **La divergencia es que el `.deb` lo
ejecuta en BUILD-time contra el árbol staged en vez de en INSTALL-time contra el árbol vivo.** El
encuadre correcto es *"de-divergir los dos instaladores llamando al merger que ya existe desde el
postinst"*, no *"tres opciones abiertas"*. **Se descarta el camino `conffile`:** en un JSON generado
deja `.dpkg-dist`/prompts y no mergea nada.

**→ Toca packaging. A charlar con el operador antes de tocarlo.**

---

<a id="u-4"></a>
## U-4 — Verificación de compatibilidad entre motherbee y spokes · **partido en dos**

> **El absoluto "cero verificación" es falso** y la remediación obvia (rechazar el peer por versión)
> **se auto-cerca la malla**. Reescrito el 2026-08-03.

**Lo que SÍ existe, y el hallazgo original negaba:** el spoke rechaza el `SYSTEM_UPDATE` con
`VERSION_MISMATCH`/`sync_pending` (ejercitado en `orchestrator_system_update_stale_e2e.sh`);
motherbee verifica el sha del manifest y de cada binario tras el push; y el runtime manifest tiene un
contrato de compatibilidad cross-hive real, con rango soportado `1..=2` y gate de escritura
`FLUXBEE_RUNTIME_MANIFEST_WRITE_V2`.

**Y la consecuencia estaba mal:** una rotura **dura** de protocolo **no es silenciosa** — el peer
queda `stale` y toda acción hacia ese hive falla con *"target hive not reachable in LSA"*. Lo que sí
degrada callado es un cambio **semántico**, porque serde tolera campos default/desconocidos. La
mitigación (payload sin cambios de protocolo, ventana corta) sigue siendo la correcta, pero **por ese
motivo**, no por "no hay ninguna red".

**Contrato de la casa, para que no se relea como omisión:** en fluxbee **el hash es la compuerta y la
versión es telemetría**. Se ve igual en el reload de OPA: versión distinta → warn, hash distinto →
rechazo.

<a id="u-4a"></a>
### U-4a 🟢 — El HELLO lleva versión de protocolo y nadie la mira

`WanHelloPayload` transporta `protocol: "fluxbee/1.16"` y existe `WanRejectPayload` (se usa para
`HIVE_NOT_AUTHORIZED`), pero **`peer_hello.protocol` no se compara nunca**, y el
`negotiated.protocol` que se devuelve es la constante local reescrita. Igual en los hermanos
(`NodeHelloPayload.version`, `RouterHelloPayload.version`). **El campo existe y está muerto.**

**⛔ Ojo con el arreglo obvio:** rechazar el peer sería **self-fencing** — el `SYSTEM_UPDATE`
cross-hive viaja por la malla y exige visibilidad LSA del target, así que **cortar el peer viejo corta
el único canal capaz de arreglarlo**. Corresponde (a) usar el campo solo para **reportar skew**, o
(b) **borrarlo del contrato** para que no simule una garantía que no da. **Decisión de diseño → hablarlo.**

<a id="u-4b"></a>
### U-4b 🔴 — Las drift-alerts **no se escriben nunca**

No son "informativas": `drift_alerts_path()` (→ `orchestrator/drift-alerts.jsonl`) **solo aparece en
la ruta de lectura**. No hay un solo escritor en todo el repo — lo verifiqué por nombre de función y
por nombre de archivo.

Lo que devuelve `GET /drift-alerts` son **entradas sintéticas** (`local_current_state`,
`severity=info`) que `enrich_drift_alert_history_entries` fabrica **a partir del snapshot actual**
cuando el hive local no tiene entradas en el archivo — o sea, **siempre**. El endpoint que el README
documenta como *"Historical drift alert entries"* devuelve **un snapshot vivo disfrazado de
historial**, que es peor que devolver vacío.

Es el bug real detrás de U-4, **y es independiente de versiones**. Falta además: `GET /versions`
agrega snapshots **sin emitir veredicto** (debería marcar qué hives difieren del de motherbee), y los
contadores de reject del WAN son un `HashMap` privado sin superficie de consulta.

**→ Escribir el emisor es diseño** (cuándo se emite, con qué severidad, cómo se deduplica, retención).
**A charlar.** Lo que sí se puede cerrar solo es la mentira: o el endpoint declara que lo que devuelve
es un snapshot, o deja de sintetizar.

---

<a id="u-5"></a>
## U-5 🟡 — No hay rollback de core como comando · **gap, no bug**

> Bajado de 🔴 el 2026-08-03: **"automático-solo" es el contrato de la familia** — vendor tampoco
> tiene comando, y runtime no lo necesita porque está versionado.

Existe rollback **automático** (local, ante fallo detectado) y hay backup de los binarios previos en
`/var/lib/fluxbee/orchestrator/core-bin.prev.local/update-<ms>/` *(ruta corregida)*. No hay acción de
rollback de core: las únicas `*_rollback` son de vault, opa y wf_rules.

**Se borró la frase sobre U-1** (está cerrado). Lo que sí se sostiene: el rollback automático se
dispara **solo si la unit no queda `active` en 30 s** — no detecta una regresión **funcional** (mesh
roto, incompatibilidad con spokes) — **y además excluye a `sy-orchestrator`**, que se auto-reinicia
sin verificación: si el binario nuevo del orchestrator crash-loopea, **no hay gate ni rollback**.

**Ya existe media máquina:** `rollback_remote_core_to_prev` restaura la generación previa completa por
SSH. Si se construye el comando, se construye **sobre eso** para spokes, no de cero.

**Y el downgrade de spokes ya es un flujo soportado:** `.deb` anterior en motherbee → dist →
`update category=core`, porque el camino core es **por hash** y no tiene gate de monotonía. Lo único
externo es conservar el `.deb`.

**⛔ Advertencia obligatoria contra el arreglo obvio.** Un `core_rollback` que *"restaure el backup más
reciente"* es **inseguro con el store actual**: (a) los directorios se crean **aunque no haya
cambios**, así que el más reciente suele estar **vacío**; (b) `rollback_local_core_binaries` devuelve
`Ok()` cuando **no encuentra** el backup — o sea, el comando contestaría `ok` sin restaurar nada,
**exactamente el modo de falla de U-1**; (c) los directorios no llevan `manifest_hash` ni versión, así
que *"rollback a la versión X"* **ni siquiera es expresable**.
*Prerrequisitos:* etiquetar el backup con el `manifest_hash` que reemplazó · no crear directorio en el
no-op · fallar fuerte si el backup no tiene los binarios · retención (reusar el patrón de runtimes).

**Límite estructural, no carencia:** una acción in-band se enruta a `SY.orchestrator@{hive}`; **no
puede rescatar al hive cuyo orchestrator es el componente roto.** Para ese caso el camino externo
(snapshot / downgrade por apt) **es la respuesta correcta**.

<a id="u-5b"></a>
### U-5b ✅ **CERRADO** — Los backups de core se acumulaban sin GC, y se creaban vacíos

Dos defectos, los dos prerrequisitos de cualquier `core_rollback` futuro:

1. **Se creaba el directorio antes de saber si había algo que respaldar** — `fs::create_dir_all` corría
   incondicionalmente, así que **cada update no-op dejaba un `update-<ms>/` vacío**. Justamente por eso
   *"restaurar el backup más reciente"* habría restaurado **nada**. Ahora la creación es **perezosa**:
   ocurre recién ante el primer binario que efectivamente se copia.
2. **Nunca se limpiaba.** Ahora hay retención (`CORE_BACKUP_GENERATIONS_KEPT = 3`), aplicada **solo en
   el camino de éxito** — un rollback todavía necesita su generación en disco. Es *best-effort*: si
   falla la limpieza, loguea `warn!` y no convierte un update bueno en un fallo reportado.

`prune_core_backup_generations_in(root, keep)` toma el root por parámetro para ser testeable sin tocar
el state dir real. Dos tests: que conserva las más nuevas y respeta lo que no es una generación, y que
**nunca vacía el store** (por debajo del umbral no toca nada, y un root inexistente no es error).


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
## U-8 — Operaciones largas sin progreso consultable · **partido en tres**

> Reescrito el 2026-08-03. El diagnóstico de U-8b señalaba una causa **falsa**, y la auditoría
> destapó en U-8c el mejor hallazgo del lote: **la maquinaria que U-8 pedía construir ya existe y
> está muerta por un bug de una línea.**

<a id="u-8a"></a>
### U-8a 🟡 — La API de admin no tiene contrato para operaciones largas

Un `add_hive` tarda **más de 6 minutos** y no hay manera de preguntarle en qué paso está: el audit log
se escribe **al terminar**, no hay acción de estado-de-operación en el registro, y el `info.yaml` del
hive nuevo (`status: pending`) se escribe **recién tras los gates de 60 s + 60 s**, así que durante la
ventana `GET /hives` no muestra nada.

**Mitigaciones que YA existen y hay que documentar en el HANDBOOK antes de codear nada:** (i) los
timeouts son perillas — `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` / `JSR_ADMIN_UPDATE_TIMEOUT_SECS`, **no
seteadas en `packaging/`**; (ii) el hive queda `pending` y reintentar `add_hive` es idempotente
*(con el matiz de [PB-7](#pb-7): solo vale si el corte cayó después de `write_hive_info`)*.

Se cierra junto con [PB-7](#pb-7) opción 2 — son el mismo hueco visto por dos puntas.

<a id="u-8b"></a>
### U-8b 🟡 — `update category=core` da TIMEOUT a los 60 s sobre una operación exitosa

**La causa que estaba escrita es falsa.** *No* es que el orquestador del destino se reinicie a sí
mismo: el código **lo evita explícitamente** — se auto-excluye y **difiere su restart a un timer de
2 s justamente para no matar su propia respuesta**.

**Las causas reales, ambas en código:** (1) el destino reinicia **primero** su propio bus
`rt-gateway`, que es por donde la respuesta debe volver, **y el fallo de envío se traga** (`let _ =`);
(2) el restart **en serie** con health-gate de 30 s por servicio sobre 8 servicios puede exceder los
60 s por sí solo. *(Aparte: si el target es motherbee, el que se reinicia a mitad del request es
`sy-admin`.)*

<a id="u-8c"></a>
### U-8c ✅ **CERRADO** — El `timeout_unknown` del architect estaba muerto por un substring imposible

> **El hallazgo de mayor valor del lote, y no estaba en ningún findings.** Lo destapó el intento de
> refutar U-8.

La semántica de *"en progreso / puede haber terminado bien / andá a verificar"* **ya existía** en
SY.architect: estado `timeout_unknown`, y `reconcile_timeout_unknown_operation`, que para `add_hive`
consulta `get_hive` y marca `succeeded_after_timeout`.

**Estaba muerta.** El clasificador matcheaba el substring `"timeout waiting ADMIN_COMMAND_RESPONSE"`
contra un `Display` que **desde d04cce8** dice:

```text
timeout waiting response trace_id=… target=… request_msg=… response_msg=ADMIN_COMMAND_RESPONSE …
```

Las palabras `waiting` y `ADMIN_COMMAND_RESPONSE` **nunca son adyacentes**. El predicado era
**permanentemente falso**: toda operación vencida se persistía como `failed` (terminal), la
reconciliación **jamás corría**, y el operador veía un `error` sobre una operación probablemente
exitosa — **exactamente el daño que U-8 describe**.

**Arreglado:** `RpcError::is_timeout()` / `timeout_response_msg()` en el SDK, y el architect matchea
la **variante tipada** vía `downcast_ref` recorriendo la cadena de `source()`, en vez de olfatear
strings. Tres tests nuevos, incluido el que faltaba: **uno que afirma que el `Display` real no
contiene el substring viejo**, para que no pueda volver a pasar en silencio.

**Nota de método:** el hermano de `sy_orchestrator` **ya lo tenía bien** —comentario que documenta la
forma del `Display`, el substring correcto y un test—. El architect era el outlier. Otra vez la regla
*"revisá los hermanos"*.

---

<a id="u-9"></a>
## U-9 ✅ **CERRADO** — Las rutas desconocidas devolvían un `not_found` pelado

`POST /hives/motherbee/hives` (mi error: la ruta correcta es `POST /hives`) devolvía exactamente
`{"error":"not_found"}`.

**El defecto primario era de ENVELOPE, no de amabilidad.** Toda otra respuesta del admin es
`{status, action, payload, error_code, error_detail}`; el catch-all emitía `{"error": …}`, **la única
clave así en toda la superficie** — un cliente que lee `error_code` recibía `null`.

**Y el caso peor, que el hallazgo no vio:** el mismo catch-all se tragaba **el método equivocado sobre
una ruta que SÍ existe** (`PUT /hives`), devolviendo *"no existe"* sobre algo que existe. Más dañino
que la falta de pistas.

**Arreglado, como reuso y no como sistema nuevo:**

- envelope de la familia + `error_code: UNKNOWN_ROUTE`;
- **405 `METHOD_NOT_ALLOWED`** con `allowed_methods` cuando la ruta existe con otro verbo, derivando
  la tabla de `admin_action_path_patterns` sobre `INTERNAL_ACTION_REGISTRY` para que no pueda
  divergir del catálogo;
- puntero `see: "GET /admin/actions"` — **un puntero, no un volcado**: el bind default es loopback
  pero es overrideable y hay reverse-proxy público documentado, y ese endpoint ya tiene su propia
  decisión de exposición;
- el path ecoado se **trunca a 256 y se limpia de caracteres de control** (viene del cliente).

Alcance exacto: el catch-all de SY.admin. **No** incluye `/modules/*`, `sy_edge` ni `io.api`, que ya
responden tipado. **El gemelo de SY.architect se dejó como está a propósito:** ahí `{"error": …}` es
lo que usan sus vecinos, así que **no es el outlier** — de-divergirlo sería romper su propia familia.

**Residual anotado, no arreglado:** `externalize` / `unexternalize` / `list_externalized` tienen rutas
REST reales pero **no están en `INTERNAL_ACTION_REGISTRY`**, así que `GET /admin/actions` no las
lista y el puntero miente para la familia `/channels/*`. Se las incluyó a mano en la tabla de
diagnóstico (`ADMIN_ROUTES_OUTSIDE_ACTION_REGISTRY`) para que el 405 funcione, pero **meterlas en el
registry toca dispatch y authz → decisión del operador.**

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

### ✅ Arreglado y VALIDADO en vivo

Se aplicó la delegación: `authorize_cloud_relay` deja pasar `externalize`/`unexternalize`
(`CHANNEL_SELF_SERVICE_ACTIONS`) hacia el handler, donde `authorize_channel_command` sigue
exigiendo `owner == caller`. **No ensancha el relay**: un IO.cloud comprometido sigue sin poder
tocar una channel ajena ni relayar nada fuera del allowlist.

**Tests que fijan el límite, no el caso feliz:** los dos comandos de channel pasan; `add_hive`,
`vault_get`, `publish_artifact` y `sync_hint` siguen denegados; y un comando de channel desde un
caller **no-IO** pasa este gate —no tiene privilegio de relay que contener— para que lo rechace el
de channel por la regla I1.

**Validación en producción** (0.1.6): se desexternalizó el canal a mano y se reinició el nodo, sin
tocar nada más.

```text
INFO  IO.cloud own channel ICH enabled — ready to externalize on SY.edge
WARN  externalize rejected by a retryable edge condition; retrying        attempts=1
INFO  externalize OK (authenticated Cloud URL published on the edge)      attempts=2
```

Ya no hay `UNAUTHORIZED`. El primer intento pegó contra un edge momentáneamente no resoluble y **el
reintento propio del nodo lo resolvió** — la lógica de retry funcionando como se diseñó. Después,
desde internet: sin credencial `401`, con bearer un `create_tenant` completo con `tenant_id` nuevo.

⇒ **El operador ya no tiene que publicar el canal a mano.** El paso 2 del checklist de
[`docs/io-cloud-api.md`](../../docs/io-cloud-api.md) §5 queda obsoleto a partir de 0.1.6.

---

<a id="u-12"></a>
## U-12 🟡 — La carrera existe, pero la operación atómica YA ESTÁ HECHA

**Corregido el 2026-08-04, el mismo día que lo reporté.** Lo planteé mal.

### Lo que es cierto

Encadenar `kill_node` y después `remove_node_instance` **sí** es una carrera: el kill responde
`status: ok` **antes** de que systemd baje la unit, y el remove inmediato falla con
`NODE_INSTANCE_RUNNING`.

Y la consecuencia también: `remove_node_instance` es lo único que borra
`/var/lib/fluxbee/nodes/<KIND>/<node>/`, así que si falla y nadie mira la respuesta, el config
queda y `reconcile_persisted_custom_nodes` **relanza el nodo** en el próximo arranque. Me pasó: di
por limpios dos nodos de prueba y reaparecieron tras el upgrade siguiente.

### Lo que estaba mal

Propuse como arreglo *"un purge atómico que haga las dos"*. **Ya existe.** `kill_node` acepta
`purge_instance`, y la propia autodocumentación lo dice:

> *"Stop a node in a hive. Optional **purge_instance** also removes its persisted instance directory."*

con el ejemplo incluido:

```bash
curl -X DELETE /hives/<hive>/nodes/<node>@<hive> -d '{"purge_instance":true}'
```

**Probado en producción**, una sola llamada:

```text
antes:    unit active    · config existe
DELETE {"purge_instance": true}  ->  status: ok
después:  unit inactive  · config BORRADO
```

Sin carrera, sin segunda llamada, sin espera.

### Lo que queda como hallazgo real

1. **La secuencia de dos llamadas sigue siendo una trampa** para quien la use — y yo la había
   escrito en el HANDBOOK como la receta de recuperación. **Corregido allí**: ahora usa
   `purge_instance`.
2. **`instance_removed` vuelve `null`** aunque el borrado ocurra. El campo está en la respuesta y
   no se completa, así que un cliente que lo chequee no puede confirmar el purgado.

### La lección, que es la tercera vez en esta sesión

Antes de proponer construir algo, **preguntarle al sistema si ya lo tiene**
(`GET /admin/actions/<accion>`). Me habría ahorrado un hallazgo mal planteado y una receta
equivocada en el handbook.
