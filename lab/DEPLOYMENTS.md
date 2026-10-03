# DEPLOYMENTS — registro de versiones desplegadas en PROD (auditoría)

> Ledger **append-only** de cada versión que TOCA producción (`pve` @ `192.168.8.207`). Es el
> registro de auditoría: **qué** versión, **cuándo**, **qué commit**, **qué cambió**, **cómo se
> verificó** y **cómo se revierte**. Una fila en la tabla + una entrada detallada por versión.
>
> **Regla (HANDBOOK §12): ningún `apt install` en prod — ni core-update a un spoke — sin una
> entrada acá.** Sin entrada, el deploy no está hecho.
>
> **Snapshots: nunca más de 3 por VM (HANDBOOK §12, desde 2026-09-25).** El rollback "por snapshot"
> de las entradas viejas puede ya no existir: el 2026-09-25 se borraron los de 0.1.19 a 0.1.29.
> El camino que siempre queda es `apt install fluxbee=<anterior>`, porque el repo conserva todas
> las versiones.
>
> Hermanos: [`logbook/HANDBOOK.md`](logbook/HANDBOOK.md) (recetas de deploy), [`logbook/METHOD.md`](logbook/METHOD.md)
> (cómo se opera la infra), `logbook/YYYY-MM-DD.md` (bitácora narrativa), [`logbook/FINDINGS.md`](logbook/FINDINGS.md)
> (hallazgos/bugs). Este doc es SÓLO el ledger de versiones en prod. Fechas en **ART (−03)**;
> el journal del host prod va en EDT (ver METHOD §1).

## Tabla rápida (auditoría)

| Versión | Fecha (ART) | Commit | Alcance | Estado | Rollback |
|---|---|---|---|---|---|
| **0.1.56** | 2026-10-03 | `715143a` | motherbee + spokes (core) | ✅ live | snap `pre-root-io-0-1-56` (las 4 VMs) · `apt install fluxbee=0.1.55` |
| **0.1.55** | 2026-10-02 | `975fd34` | motherbee + spokes (core) | ✅ live | snap `pre-root-tenant-0-1-55` (las 4 VMs) · `apt install fluxbee=0.1.54` |
| **0.1.54** | 2026-10-02 | `6aa4ee4` | motherbee + spokes (core) | ✅ live | snap `pre-gov-0-1-54` (las 4 VMs) · `apt install fluxbee=0.1.53` |
| **0.1.53** | 2026-10-02 | `26edcd5` | motherbee + spokes (core) | ✅ live | snap `pre-ai-generic-0-1-53` (las 4 VMs) · `apt install fluxbee=0.1.52` |
| **0.1.52** | 2026-10-02 | `0670184` | motherbee + spokes (core) | ✅ live | snap `pre-cleanup-0-1-52` (las 4 VMs) · `apt install fluxbee=0.1.51` |
| **0.1.51** | 2026-10-01 | `f7c2520` | motherbee + spokes (core) | ✅ live | snap `pre-query-decode-0-1-51` (las 4 VMs) · `apt install fluxbee=0.1.50` |
| **0.1.50** | 2026-10-01 | `aa206ac` | motherbee + spokes (core) | ✅ live | snap `pre-config-routes-0-1-50` (las 4 VMs) · `apt install fluxbee=0.1.49` |
| **0.1.49** | 2026-10-01 | `c921d1a` | motherbee + spokes (core) | ✅ live | snap `pre-architect-fixes-0-1-49` (las 4 VMs) · `apt install fluxbee=0.1.48` |
| **0.1.48** | 2026-10-01 | `1486807` | motherbee + spokes (core) | ✅ live | snap `pre-architect-opa-0-1-48` (las 4 VMs) · `apt install fluxbee=0.1.47` |
| **0.1.47** | 2026-10-01 | `652a9c9` | motherbee + spokes (core) | ✅ live | snap `pre-opa-blockB-0-1-47` (las 4 VMs) · `apt install fluxbee=0.1.46` |
| **0.1.46** | 2026-10-01 | `7371ca1` | motherbee + spokes (core) | ✅ live | snap `pre-opa-blockA-0-1-46` (las 4 VMs) · `apt install fluxbee=0.1.45` |
| **0.1.45** | 2026-10-01 | `d6505ff` | motherbee + spokes (core) | ✅ live | snap `pre-rule3-0-1-45` (las 4 VMs) · `apt install fluxbee=0.1.44` |
| **0.1.44** | 2026-10-01 | `005d0ad` | motherbee + spokes (core) | ✅ live | snap `pre-src-binding-0-1-44` (las 4 VMs) · `apt install fluxbee=0.1.43` |
| **0.1.43** | 2026-09-30 | `2b3abb2` | motherbee + spokes (core) | ✅ live | snap `pre-sync-hint-0-1-43` (las 4 VMs) · `apt install fluxbee=0.1.42` |
| **0.1.42** | 2026-09-30 | `ac301d9` | motherbee + spokes (core) | ⚠️ superada (escaneo rechazado: 0.1.43) | snap `pre-opa-fixes-0-1-42` (las 4 VMs) · `apt install fluxbee=0.1.41` |
| **0.1.41** | 2026-09-30 | `72891be` | motherbee + spokes (core) + `hive.yaml` | ✅ live | snap `pre-opa-global-0-1-41` (las 4 VMs) · `apt install fluxbee=0.1.40` + `hive.yaml.pre-opa-0-1-41` |
| **0.1.40** | 2026-09-30 | `43dd174` | motherbee + spokes (core) | ✅ live | snap `pre-purge-order-0-1-40` (las 4 VMs) · `apt install fluxbee=0.1.39` |
| **0.1.39** | 2026-09-30 | `85c3244` | motherbee + spokes (core) | ✅ live | snap `pre-identity-baseline-0-1-39` (las 4 VMs) · `apt install fluxbee=0.1.38` |
| **0.1.38** | 2026-09-30 | `20baec3` | motherbee + spokes (core) | ✅ live | snap `pre-wf-mirror-0-1-38` (las 4 VMs) · `apt install fluxbee=0.1.37` |
| **0.1.37** | 2026-09-30 | `c09f9af` | motherbee + spokes (core) | ✅ live | snap `pre-followups-0-1-37` (las 4 VMs) · `apt install fluxbee=0.1.36` |
| **0.1.36** | 2026-09-30 | `d24e3aa` | motherbee + spokes (core) | ✅ live | snap `pre-config-scope-0-1-36` (las 4 VMs) · `apt install fluxbee=0.1.35` |
| **0.1.35** | 2026-09-28 | `5f1cd71` | motherbee | ✅ live | snap `pre-teardown-fix-0-1-35` · `apt install fluxbee=0.1.34` |
| **0.1.34** | 2026-09-28 | `60331c5` | motherbee + spokes (core) | ⚠️ superada (regresión teardown) | snap `pre-lifecycle-0-1-34` (las 4 VMs) · `apt install fluxbee=0.1.33` |
| **0.1.33** | 2026-08-28 | `01b6266` | motherbee | ✅ live | `apt install fluxbee=0.1.32` |
| **0.1.32** | 2026-08-27 | `90fd64a` | motherbee | ✅ live | `apt install fluxbee=0.1.31` |
| **0.1.31** | 2026-08-26 | `862c5e4` | motherbee | ✅ live | snap `pre-rpc-poison-fix-0-1-31` · `apt install fluxbee=0.1.30` |
| **0.1.30** | 2026-08-26 | `af1166a` | motherbee | ✅ live | snap `pre-liveness-fix-0-1-30` · `apt install fluxbee=0.1.29` |
| **0.1.29** | 2026-08-25 | `a0bc25d` | motherbee | ✅ live | snap `pre-cloud-readpath-0-1-29` · `apt install fluxbee=0.1.28` |
| **0.1.28** | 2026-08-25 | `1438737` | motherbee | ✅ live | snap `pre-frontdesk-configplane-0-1-28` · `apt install fluxbee=0.1.27` |
| **0.1.27** | 2026-08-25 | `000de90` | motherbee | ✅ live | snap `pre-frontdesk-autonomous-0-1-27` · `apt install fluxbee=0.1.26` |
| **0.1.26** | 2026-08-24 | `cb2d192` | motherbee | ✅ live | snap `pre-frontdesk-handoff-0-1-26` · `apt install fluxbee=0.1.25` |
| **0.1.25** | 2026-08-24 | `69b1c46` | motherbee | ✅ live | snap `pre-observability-0-1-25` · `apt install fluxbee=0.1.24` |
| **0.1.24** | 2026-08-23 | `35349ef` | motherbee | ✅ live | snap `pre-vault-guard-0-1-24` · `apt install fluxbee=0.1.23` |
| **0.1.23** | 2026-08-21 | `4abad1b` | motherbee | ✅ live | snap `pre-cloud-actions-0-1-23` · `apt install fluxbee=0.1.22` |
| **0.1.22** | 2026-08-20 | `15fc77f` | motherbee | ✅ live | snap `pre-frontdesk-0-1-22` · `apt install fluxbee=0.1.21` |
| **0.1.21** | 2026-08-20 | `2f60403` | motherbee | ✅ live | snap `pre-register-human-0-1-21` · `apt install fluxbee=0.1.20` |
| **0.1.20** | 2026-08-20 | `1297ba7` | motherbee | ✅ live | snap `pre-router-0-1-20` · `apt install fluxbee=0.1.19` |
| ≤ 0.1.19 | (pre-ledger) | — | motherbee | histórico, sin registrar | repo apt conserva 0.1.0 … 0.1.19 |

> Este ledger arranca en **0.1.20** (primera vez que se registra formalmente). Las versiones
> anteriores (0.1.0–0.1.19) se desplegaron sin ledger; el repo apt en fb-build las conserva
> (`dpkg-scanpackages -m`) para rollback, pero su detalle vive en la bitácora, no acá.

---

## 0.1.56 — una conexión por UUID, upgrades sin apagar el core, solo io.cloud e io.blob en el tenant raíz

- **Fecha:** 2026-10-03 (ART) · **Versión anterior:** 0.1.55 · **Commits:** `068014d`..`715143a`
  (FINDINGS A-16, A-43, A-44, A-45, B-16; bitácora `2026-10-03`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Antes del deploy, a mano en PROD (el operador, porque el clasificador de permisos se lo
  bloqueó a la sesión):** se purgaron `IO.api@motherbee` e `IO.wapp.default@motherbee`, con sus
  ILKs y sus archivos UUID. Sus configs quedaron guardadas fuera del repo; solo tenían `_system` y
  `tenant_id`.
- **Qué cambió:**
  - **A-44:**
    - el router acepta una sola conexión viva por UUID, con 1 s de espera para los duplicados
      pasajeros;
    - toda conexión que termina se limpia, incluso por error (antes quedaba registrada y sin
      lectura).
  - **A-45:** un upgrade solo para a los que escriben el manifest de runtimes. El orchestrator
    nuevo reinicia en orden lo que corre binarios reemplazados, y needrestart deja el core al
    orchestrator.
  - **A-43:** solo io.cloud e io.blob corren en el tenant raíz y arrancan por defecto
    (`TENANT_ROOT_NOT_ALLOWED`).
  - **A-16:** el watchdog revierte los cambios locales de las carpetas receive-only.
- **Build:** 20,5 min. **Publish:** 11 s, 56 paquetes. **Deploy:** 214 s con `ops deploy`;
  snapshots `pre-root-io-0-1-56` en las 4 VMs (se borró antes `pre-ai-generic-0-1-53`).
- **Incidente del deploy (B-16):**
  - Los core updates salieron antes de que el router del motherbee tuviera de vuelta a los spokes.
    Los tres dieron `TRANSPORT_ERROR` y no llegaron.
  - Igual `ops.py` los dio por actualizados: juzgaba por el hash de dist, no por el instalado.
  - El chequeo de binarios lo mostró. El update se volvió a mandar a mano a las 06:39:52 UTC y
    `ops.py` quedó corregido.
- **Verificación en vivo:**
  - **U-8b validado:** los tres spokes contestaron `status=ok phase=restarting` (9, 5 y 4
    componentes) y terminaron en `core.last_update` `done ok`. worker1 reinició los servicios en
    orden y después se reinició a sí mismo.
  - Los 4 hives en 0.1.56: el motherbee por dist y los spokes por hash instalado (`3260c886…`).
    Ningún binario `(deleted)` y 0 líneas ERROR, en los 4.
  - **A-39 sigue bien:** IO.blob e IO.cloud arrancaron una sola vez, en 0.1.56, con cero
    `203/EXEC`. Son los únicos nodos administrados que quedan en el motherbee.
  - **A-44:** 0 HELLO rechazados, 0 conexiones terminadas por error y 0 handshakes fallidos en el
    tráfico normal de los 4 hives. Los nodos Go arrancaron bien con su doble HELLO.
  - **A-43:** un `run_node` de `IO.api.a43probe@motherbee` en el tenant raíz dio
    `TENANT_ROOT_NOT_ALLOWED`, sin dejar nada creado (ni ILK, ni archivo UUID, ni unit).
  - **A-45:**
    - el `prerm` nuevo y la conf de needrestart quedaron instalados;
    - este upgrade todavía paró todo el core, entre 06:33:26 y 06:34:58, porque corre el `prerm`
      de 0.1.55;
    - se mide en el próximo upgrade.
  - **A-16:** no se probó en vivo. Hacerlo implica escribir en un espejo de un spoke, y queda a
    decisión del operador.
  - **Identity y frontdesk:** 0 errores. La réplica de worker1 hizo el full sync después de su
    reinicio.
  - **CI:** `rust-tests`, `admin-catalog-guard` y `router-dispatcher-guards` en verde en `715143a`.
- **Error propio, sin consecuencias:**
  - Para leer la versión corrí `/usr/bin/sy-orchestrator --version` en worker1. El binario no
    tiene ese flag y arrancó como orchestrator.
  - Murió antes de conectarse al router: no hubo registro nuevo en el router de worker1 y el
    orchestrator real siguió accesible.
  - No hay que correr binarios del core para consultar versiones; se usa `/versions`.
- **Rollback:** snapshot `pre-root-io-0-1-56` o `apt install fluxbee=0.1.55` (los spokes, por core
  update o `core_rollback`).

## 0.1.55 — nadie se registra en el tenant raíz, U-8b, io.slack sin instancia de base

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.54 · **Commits:** `a8c6349`..`975fd34`
  (FINDINGS A-37, A-40, A-43, A-44, A-45; PENDING-BUGS U-8b; bitácora `2026-10-02`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Antes del deploy, a mano en PROD (pedido del operador):** se borró `IO.slack.default@motherbee`
  con purga; corría en el tenant raíz y no tenía su ILK (A-40). Su config quedó guardada fuera del
  repo.
- **Qué cambió:**
  - **Tenant raíz:** nadie se registra ahí. SY.identity lo rechaza para cualquier llamador
    (`TENANT_ROOT_NOT_REGISTRABLE`), el frontdesk responde `TENANT_NOT_REGISTRABLE` sin LLM y
    `register_human` lo rechaza antes de provisionar.
  - **U-8b:** el core update de un spoke contesta `phase: restarting` antes de reiniciar, y deja el
    resultado en `/versions` como `core.last_update`.
  - **io.slack:** sin instancia de arranque; cada binding lo lanza su tenant.
  - **Gate de frontdesk de io.slack/io.wapp:** el handoff va como mensaje `user`; como `data`, el
    frontdesk lo ignoraba.
  - **SHM del router (A-44):** un nodo que se registra de nuevo conserva su slot, y el lector ya no
    pierde el último nodo cuando hay un hueco.
  - **Lab:** los e2e de identity crean sus tenants por SY.admin.
- **Build:** 17,5 min. **Publish:** 10 s, 55 paquetes. **Deploy:** 233 s con `ops deploy`;
  snapshots `pre-root-tenant-0-1-55` en las 4 VMs (se borró antes `pre-cleanup-0-1-52`).
- **Verificación en vivo:**
  - Los 4 hives en 0.1.55 con el mismo manifest (`852a21cd…`). Ningún binario `(deleted)` y 0
    líneas ERROR desde el install, en los 4.
  - **A-39 sigue bien:** IO.api, IO.blob, IO.cloud e IO.wapp.default arrancaron una sola vez, en
    0.1.55, con cero `203/EXEC`; el orchestrator los reapuntó.
  - **Identity y frontdesk:** 0 errores. La réplica de worker1 hizo el full sync y se suscribió a
    los deltas.
  - **U-8b, todavía no:** los spokes recibieron el update con el orchestrator 0.1.54, así que
    ingress1 y egress1 dieron `TIMEOUT` como antes (worker1 contestó a tiempo).
    `core.last_update` está en `/versions`, en `null`. La respuesta antes del reinicio se ve en el
    próximo release.
  - **No probado en vivo:** el bloqueo del tenant raíz, que necesitaría registrar a alguien en PROD.
    Lo cubren los tests y el e2e `identity_negative`.
  - **Visto al verificar (A-45, preexistente):** el upgrade del motherbee deja todo el core caído
    70–86 s, porque el `prerm` para los servicios antes de desempaquetar. Las réplicas lo muestran
    como ~2 avisos de reconexión por segundo.
  - **CI:** `rust-tests` y `router-dispatcher-guards` en verde en `975fd34`.
- **Rollback:** snapshot `pre-root-tenant-0-1-55` o `apt install fluxbee=0.1.54`.

## 0.1.54 — upgrades sin caída de los nodos administrados, el camino .gov cerrado, cognition A-38

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.53 · **Commits:** `7c9f781`..`6aa4ee4`
  (FINDINGS A-37, A-38, A-39, A-41; bitácora `2026-10-02`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Antes del deploy, a mano en PROD (pedido del operador):**
  - se borró `AI.chat@motherbee` con purga (instancia e ILK);
  - se le quitó a IO.slack.default su `io.dst_node`, que apuntaba a AI.chat (config v4);
  - se borraron 12 archivos `.uuid` de nodos ya borrados.
- **Qué cambió:**
  - **A-39:** el arranque del orchestrator relanza en la versión nueva a los nodos que siguen a
    `current`, y `needrestart` ya no reinicia las units `fluxbee-node-*`.
  - **A-41:** la purga de un nodo borra su UUID.
  - **A-37, .gov:**
    - el registro va al tenant del caso y el frontdesk no crea tenants;
    - merge por email: completa solo campos vacíos y mueve los canales;
    - `TENANT_SUSPENDED`;
    - `register_human` informa el ILK final.
  - **A-38, cognition:** `context_close`/`reason_close` y lo reciente primero en la región de memoria.
- **Build:** 13,5 min. **Publish:** 11 s, 54 paquetes. **Deploy:** 231 s con `ops deploy`; snapshots
  `pre-gov-0-1-54` en las 4 VMs (se borró antes `pre-query-decode-0-1-51`).
- **Verificación en vivo:**
  - Los 4 hives en 0.1.54 con el mismo manifest (`7b87d5c1…`). Ningún binario `(deleted)`, 0 líneas
    ERROR desde el install y 0 rechazos del gate y descartes por `routing.src`.
  - **A-39 validado:**
    - los cinco nodos administrados arrancaron una sola vez, en 0.1.54, con cero `203/EXEC`;
    - el orchestrator los reapuntó ("running node follows 'current' and the pointer moved");
    - `/etc/needrestart/conf.d/fluxbee.conf` quedó instalado.
  - **Cognition:** rechazó los umbrales viejos de su config persistida (warning) y aplica los
    defaults; CONFIG_GET informa `context_close`/`reason_close` 0,25, el mismo efecto que antes.
  - **Identity y frontdesk:** 0 errores desde el install.
  - **CI:** `rust-tests` y los guardianes en verde en `6aa4ee4`.
- **Rollback:** snapshot `pre-gov-0-1-54` o `apt install fluxbee=0.1.53`.

## 0.1.53 — limpieza de código muerto: la clave general de IA, sin instancia AI de base, privacidad y cognition

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.52 · **Commits:** `8c8c0fa`..`26edcd5` (ocho,
  uno por tema; bitácora `2026-10-02`, FINDINGS A-32 a A-38).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **ai.generic** toma la clave general del proveedor del hive en SY.vault (decisión D4 del
    operador). `behavior.vault_key` se rechaza (A-35). Sale el modo gov que quedó del split de
    frontdesk.
  - **La instalación base** ya no levanta `AI.chat@motherbee`. Todo apunta a `ai.generic`: admin,
    Archi, semillas del cookbook y docs (A-36).
  - **SY.frontdesk.gov** sin datos personales en los logs (A-32). Los rechazos de identity conservan
    su código y solo lo transitorio es reintentable (A-33).
  - **SY.identity:** no loguea la dirección en `ILK_PROVISION` ni devuelve el DETAIL de Postgres (A-32).
  - **SY.cognition:** el arranque en frío corre después de esperar al vault y reconstruye con las
    claves del camino en vivo; el estado queda acotado y los scopes se cortan (A-34).
  - **Orchestrator:** 9 funciones muertas menos. Warnings del workspace: 0.
  - **CI:** las actions sobre Node 24.
- **Build:** 14 min. **Publish:** 10 s, 53 paquetes. **Deploy:** 223 s con `ops deploy`; snapshots
  `pre-ai-generic-0-1-53` en las 4 VMs (se borró antes `pre-config-routes-0-1-50`).
- **Verificación en vivo:**
  - Los 4 hives en 0.1.53 con el mismo manifest (`dc605aa6…`). Ningún binario `(deleted)`, 0 líneas
    ERROR desde el install y 0 rechazos del gate y descartes por `routing.src`.
  - **Cognition:** el rebuild de arranque corrió por primera vez (`trigger="startup"`, después de
    esperar al vault; 0 hilos, PROD no tiene datos). CONFIG_GET informa `storage.db_configured: true`.
  - **AI.chat:** arranca en FAILED_CONFIG con *"config.json rejected… send a valid CONFIG_SET"*,
    porque su config tiene `vault_key`. Es lo esperado (A-36).
  - **SY.frontdesk.gov:** arranca y queda degradado (Unconfigured): el vault de PROD no tiene clave
    de IA. El handoff estructurado funciona sin ella.
  - **CI:** `rust-tests`, `go-tests` y los dos guardianes en verde en `26edcd5`.
- **Visto al validar (A-39, preexistente):** los seis nodos administrados de motherbee quedaron
  entre 20 y 80 s en loop `203/EXEC` durante el update, porque el directorio de su versión vieja se
  borró antes de que la unit apuntara a la nueva.
- **Rollback:** snapshot `pre-ai-generic-0-1-53` o `apt install fluxbee=0.1.52`.

## 0.1.52 — limpieza de warnings sin cambio de comportamiento; primer deploy con `lab/ops.py`

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.51 · **Commit:** `0670184` (el código
  es `e1f0323`; el resto son herramientas y documentos del lote de temas chicos, bitácora
  `2026-10-02`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Nada de comportamiento: fuera campos que nadie leía, accesores de SHM sin uso e imports; tres
    costuras de test pasan a `cfg(test)`. Warnings del workspace raíz: 64 → 31 (lo que queda es
    código muerto que espera la aprobación del operador). El layout de la región de identity da el
    mismo `total_len` en el SDK y en el router.
  - Fuera del `.deb`: `lab/ops.py` (build, publish, deploy) y el publish incremental (`fcb5af6`).
- **Build:** 21 min con `ops build 0.1.52 --wait`. **Publish:** 9 s, 52 paquetes.
- **Deploy:** `ops deploy 0.1.52 --snapshot pre-cleanup-0-1-52 --drop pre-architect-fixes-0-1-49`,
  226 s de punta a punta: snapshots en las 4 VMs, install en motherbee a las 10:24 UTC, spokes en
  paralelo, health.
- **Verificación en vivo:**
  - Los 4 hives en 0.1.52 con el mismo manifest (`250e0aca…`). Ninguna unit corre un binario
    `(deleted)` y todas reiniciaron después del install, salvo `fluxbee-syncthing` (el core-update
    no la toca).
  - 0 líneas ERROR en los journals de las 4 VMs desde el install. Los WARN son de los reinicios
    (RPC y WAN que reconectan) y los ya conocidos (io_slack sin credenciales,
    `EGRESS_MOTHERBEE_BYPASS`).
  - Identity: la réplica de worker1 reintentó mientras el identity de motherbee reiniciaba
    (10:25:21–10:27:03 UTC). Después aplicó el full sync, se suscribió al delta stream y resolvió
    su ILK.
  - OPA: los 4 hives `in_sync` y los routers `ok` (sin policy de usuario cargada). La policy de
    sistema autorizó los core-updates.
  - 0 rechazos del gate y 0 descartes por `routing.src`.
  - CI: `rust-tests`, `admin-catalog-guard` y `router-dispatcher-guards` en verde en `0670184`.
- **Rollback:** snapshot `pre-cleanup-0-1-52` o `apt install fluxbee=0.1.51`.

## 0.1.51 — el admin decodifica la query; SQLite espera el lock en todas las conexiones

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.50 · **Commits:** `9f9982a` (admin) y
  `f7c2520` (sy-timer, wf-generic) (FINDINGS A-30, A-29). La primera build de 0.1.51 (solo con el
  admin) se rehízo antes de publicarla para sumar el arreglo de SQLite.
- **Alcance:** motherbee + los tres spokes (core-update); wf-generic llega como runtime
  `wf.engine/0.1.51`.
- **Qué cambió:**
  - El admin decodifica `%xx` en la query (`+` queda literal).
  - sy-timer y wf-generic aplican sus pragmas de SQLite en cada conexión (en el DSN), no solo en la
    primera del pool.
- **Build:** 6 min. **Publish:** 51 paquetes. Snapshots `pre-query-decode-0-1-51` en las 4 VMs (se
  borró antes `pre-architect-opa-0-1-48`).
- **Install:** motherbee 00:48 UTC del 02/10; spokes a continuación.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Tap creado y borrado con `match_src=IO.pct-a%40motherbee...` → `ok`, el tap ya no está (en
    0.1.50 respondía `NOT_FOUND`).
  - sy-timer activo en motherbee y worker1; `TIMER_LIST` por el admin responde; 0 errores,
    `locked` o `busy` en sus logs.
  - WF de prueba en worker1: creado, el nodo arranca desde `wf.engine/0.1.51` en ~10 s, abre su
    base y recupera 0 instancias sin errores; borrado y el nodo desaparece.
  - 0 rechazos del gate y 0 descartes por `routing.src`.
  - CI: `go-tests` (con la suite de sy-timer), `rust-tests`, `admin-catalog-guard` y
    `router-dispatcher-guards` en verde en los commits de esta versión.
- **Rollback:** snapshot `pre-query-decode-0-1-51` o `apt install fluxbee=0.1.50`.

## 0.1.50 — config-routes cerrado (VPN por hive) y lo que quedaba de OPA

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.49 · **Commit:** `aa206ac` (FINDINGS
  A-18, A-21; panel DTAP D-12). Cambian el router (`rt-gateway`), el admin y la policy de sistema.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Una sola superficie para rutas, VPN y taps: `/hives/{h}/routes|vpns|taps`. Se borraron
    `/routes`, `/vpns` y `/taps` con `?hive=`.
  - La regla 5 de `system.rego` ya no nombra `SY.config-routes`; `system.wasm` y
    `system_route.wasm` recompiladas.
  - El router relee la config de ruteo en cada latido, con control de versión (D-12).
  - Un router sin policy de usuario reporta `ok`, como después de un clear (A-21).
  - `OPA_RELOAD` ya no se reenvía a todos los nodos locales: solo lo usan los routers.
- **Build:** 23 min (cambió la librería del router: se recompila todo). **Publish:** 50 paquetes.
  Snapshots `pre-config-routes-0-1-50` en las 4 VMs (se borró antes `pre-opa-blockB-0-1-47`).
- **Install:** motherbee 00:04 UTC del 02/10; spokes 00:05–00:07.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Recién arrancados y sin policy, los 4 routers reportan `load=0 ok` (antes `load=1 error`).
  - `/routes`, `/vpns` y `/taps` → `UNKNOWN_ROUTE`; `/hives/{h}/...` responde.
  - Ruta de prueba en worker1 y tap de prueba en el motherbee por las rutas por hive: se listan, y
    cada una aparece en el LSA del otro hive. Borradas al terminar.
  - Apply y clear globales: 4,9 s y 5,0 s, los 4 hives. `OPA_RELOAD` aplicado en cada router y
    ningún "forwarded to local nodes". El refresco en el latido solo registra cuando la config
    cambia (motherbee: 4 actualizaciones en la ventana; logs a ritmo normal).
  - 0 rechazos del gate y 0 descartes por `routing.src`.
  - Visto de paso: un DELETE de tap con `@` codificado como `%40` no encontraba el tap (el admin no
    decodificaba la query). Arreglado en 0.1.51.
- **Rollback:** snapshot `pre-config-routes-0-1-50` o `apt install fluxbee=0.1.49`.

## 0.1.49 — arquitecto: un solo admin, el handbook en el `.deb`, el vault esperado al arrancar

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.48 · **Commit:** `c921d1a`
  (FINDINGS A-24, A-25, A-26). Cambian `sy-architect` y `build-deb.sh`.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - El snapshot y la consulta `query_hive` del plan compiler le preguntan a `SY.admin@motherbee`
    con el hive como destino (antes, a `SY.admin@<hive>`, que solo existe en el motherbee).
  - El `.deb` instala `/etc/fluxbee/handbook_fluxbee.md`, como `install.sh`.
  - Al arrancar, el arquitecto reintenta leer su clave de IA y la URL de su base de mensajes hasta
    que el vault conteste (como SY.storage y SY.identity), las dos a la vez.
- **Build:** 10 min. **Publish:** 49 paquetes (la espera de `pve.py` cortó por conexión; el repo
  quedó con 0.1.49). Snapshots `pre-architect-fixes-0-1-49` en las 4 VMs (se borró antes
  `pre-opa-blockA-0-1-46`).
- **Install:** motherbee 21:59 UTC; spokes 21:59–22:01.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Handbook en `/etc/fluxbee/handbook_fluxbee.md` (30940 bytes).
  - Arranque del arquitecto: las dos lecturas del vault dieron `vault not reachable yet; retrying` y
    1,2 s después escuchaba con `messages_db_configured=true` (en 0.1.48 arrancaba en `false` y lo
    rescataba el aviso del vault). `ai_configured=false`: el vault de 8.x no tiene clave de IA.
  - El admin del motherbee contesta las cinco lecturas del snapshot para worker1. En ingress1 y
    egress1 fallan `runtimes` y `wf-rules` (por rol): queda anotado en A-24.
  - Apply y clear globales: 5,1 s y 5,3 s, los 4 hives. 0 rechazos del gate y 0 descartes por
    `routing.src`; sin policy de usuario al terminar.
- **Rollback:** snapshot `pre-architect-fixes-0-1-49` o `apt install fluxbee=0.1.48`.

## 0.1.48 — el arquitecto declara la policy OPA global (una por sistema, gana la última)

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.47 · **Commit:** `1486807`
  (FINDINGS A-23). Solo cambia `sy-architect`.
- **Alcance:** motherbee + los tres spokes (core-update; el arquitecto corre solo en el motherbee).
- **Qué cambió:**
  - Una solución declara `desired_state.opa` (una policy, sin hive) en lugar de `opa_deployments`
    por hive. Una policy con `hive` no pasa la validación.
  - El snapshot lee la policy que corre una sola vez, del motherbee. Si el rego es el mismo, no hay
    cambio.
  - Un solo `opa_compile_apply`. `OPA_REMOVE` ahora es `opa_clear`.
  - Si dos soluciones declaran OPA, gana la última. El dueño de la policy que corre es la solución
    cuyo manifest guardado la declaró más recientemente. Una solución solo hace clear si la policy
    sigue siendo suya, y el plan (`user_policy` en la confirmación) dice de quién es la que
    reemplaza.
  - El plan compiler recibe el rego declarado y los pasos OPA del plan quedan fijados a él (rego y
    entrypoint exactos, sin hive).
- **Build:** 12 min. **Publish:** 48 paquetes. Snapshots `pre-architect-opa-0-1-48` en las 4 VMs
  (se borró antes `pre-rule3-0-1-45`).
- **Install:** motherbee 21:19 UTC; spokes 21:19–21:21.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`; el arquitecto activo y `/api/status` ok.
  - La respuesta real de `opa_get_policy` del motherbee, con y sin policy, se capturó y quedó como
    test del parser del snapshot.
  - Apply y clear globales: 6,2 s y 4,9 s, los 4 hives. 0 rechazos del gate y 0 descartes por
    `routing.src`; sin policy de usuario al terminar.
  - **No probado en vivo:** el pipeline completo (el diseñador y el plan compiler son IA, y el
    arquitecto arrancó sin IA: A-26). Cubierto por tests: validación, snapshot, reglas de dueño,
    fijado de pasos y confirmación.
- **Rollback:** snapshot `pre-architect-opa-0-1-48` o `apt install fluxbee=0.1.47`.

## 0.1.47 — OPA, bloque B del panel DTAP: un hive trabado dice por qué, se reparan instalaciones a medias, sin carrera en el estado, sin rego viejo

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.46 · **Commit:** `652a9c9` (panel DTAP
  2026-10-01: P-6, P-7, P-13, P-15, D-9, D-10). Solo cambia `sy-opa-rules` (Go).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - `get_status` de SY.opa.rules dice qué corre el hive, qué le llegó publicado
    (`published_version`/`published_hash`), qué tiene cargado el router (`region_hash`), si está
    `in_sync`, y si espera algo: `waiting {version, hash, since, reason}`. Al minuto de estar
    trabado lo loguea una vez.
  - Cada chequeo de 5 s (motherbee incluido) reescribe la región del router si no tiene la policy
    instalada (instalación cortada a mitad de camino).
  - El estado tiene su propio lock (antes, `lastError` sin lock entre dos goroutines).
  - Instalar sin rego borra el `policy.rego` que había dejado una policy anterior.
  - Los archivos publicados quedan con el dueño de `dist/policy`, no con un `fluxbee` fijo.
- **Build:** 7 min. **Publish:** 47 paquetes. Snapshots `pre-opa-blockB-0-1-47` en las 4 VMs (se
  borró antes `pre-src-binding-0-1-44`).
- **Install:** motherbee 19:55 UTC; spokes 19:56–19:57.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Sin policy: los 4 con `in_sync: true` y la región en ceros. Apply global: 4,7 s, los 4 con
    `in_sync: true` y la región igual al hash publicado.
  - En el motherbee, `dist/policy/opa/*` es `fluxbee:fluxbee` y solo queda el wasm vigente. Las
    réplicas no tienen `policy.rego`.
  - Hive trabado, forzado en egress1: Syncthing cortado (nft en :22000), apply de la v18 (12 s,
    `pending: [egress1]`, `unreachable: []`) y el manifest del motherbee copiado a mano en la copia
    local (llega el manifest y no el wasm). Estado: `in_sync: false`,
    `waiting {version: 18, reason: "the wasm the manifest names has not arrived"}`; el log a
    1m0s; al desbloquear, Syncthing reconectó en ~1:40 y egress1 instaló solo
    (`installed after waiting 2m51s`). Revert de la carpeta (`/rest/db/revert`) →
    `receiveOnlyTotalItems: 0`.
  - Con el sync cortado del todo (sin la copia a mano), egress1 se ve `in_sync: true`: compara con
    lo que le llegó. Lo que dice que está atrasado es el `pending` del reporte del admin.
  - Clear final: 5,3 s, los 4 sin policy. 0 rechazos del gate y 0 descartes por `routing.src` (los
    únicos "matches" del journal eran el log del guest-agent con el propio comando de conteo).
  - No probado en vivo (cubierto por tests): la reparación de la región a medias y la carrera
    (`-race`).
- **Rollback:** snapshot `pre-opa-blockB-0-1-47` o `apt install fluxbee=0.1.46`.

## 0.1.46 — OPA, bloque A del panel DTAP: arquitecto, reporte honesto, esperas acotadas, re-publicación, una sola vía de escritura

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.45 · **Commit:** `7371ca1` (panel DTAP
  2026-10-01: D-1…D-8, P-4, P-5, P-12, P-14, A-1…A-3, T-5, T-11, T-13)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - El arquitecto traduce las escrituras OPA globales (`/opa/policy*`, incluido clear) y les da 80 s.
  - El admin espera 10 s las respuestas de los hives (antes 30) y como mucho 5 s el escaneo; si el
    motherbee no contesta el paso que cambia la policy, responde `TIMEOUT` (antes `200 ok`).
  - El reporte espera a todos los hives `connected` del registro (alcanzables o no), marca
    `unreachable` y solo cuenta la respuesta de `SY.opa.rules@<hive>` con el hash anunciado.
  - El motherbee re-publica si el manifest no nombra la policy que corre.
  - `CONFIG_SET` sobre SY.opa.rules es de solo lectura.
- **Build:** 11 min. **Publish:** 46 paquetes. Snapshots `pre-opa-blockA-0-1-46` en las 4 VMs (se
  borró antes `pre-sync-hint-0-1-43`).
- **Install:** motherbee 18:47 UTC; spokes 18:48–18:52.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - `CONFIG_SET` con `compile_apply` → `UNSUPPORTED_OPERATION`; la policy no cambió.
  - Apply y clear globales: ~5 s, los 4 hives del registro en `running_hives`, `unreachable` vacío.
  - Con Syncthing de egress1 cortado (nft en :22000): la escritura volvió en 13,6 s (antes ≥32 s)
    con `pending: [egress1]` y `unreachable: []`; al desbloquear, egress1 se puso al día en 7 s.
  - Manifest del motherbee alterado a mano (hash falso): re-publicado en 5,0 s.
  - 0 rechazos del gate y 0 descartes por `routing.src` en los 4 routers; sin policy de usuario al
    terminar.
  - No probado en vivo (cubierto por tests): la traducción del arquitecto, el `TIMEOUT` del paso de
    cambio y la respuesta `PUBLISH_FAILED`.
- **Rollback:** snapshot `pre-opa-blockA-0-1-46` o `apt install fluxbee=0.1.45`.

## 0.1.45 — el orquestador solo puede mandar lo que reenvía; fuera las señales de config muertas

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.44 · **Commit:** `d6505ff` (FINDINGS A-20)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Regla 3 de la policy de sistema: un `SY.orchestrator` solo puede mandar las 16 acciones que de
    verdad reenvía a otro hive (antes, todas las protegidas salvo edge). Fuera quedan `CONFIG_SET`,
    `CONFIG_GET`, `CONFIG_CHANGED`, `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT` e `INVENTORY_REQUEST`.
  - Borrado lo que nadie usaba: el `CONFIG_CHANGED {node_config}` del orquestador y el flag
    `notify`; el `CONFIG_RESPONSE` extra después de `set_storage`; el handler de CONFIG_CHANGED de
    sy-wf-rules (aplicaba workflows sin chequear origen); la rama de io-slack. CONFIG_CHANGED queda
    con un emisor (SY.admin) y un receptor (SY.opa.rules).
- **Build:** 17 min. **Publish:** 45 paquetes. Snapshots `pre-rule3-0-1-45` en las 4 VMs (se borró
  antes `pre-opa-fixes-0-1-42`).
- **Install:** motherbee 15:22 UTC; spokes 15:25–15:28.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - WF en worker1 (lo que más reenvíos entre orquestadores ejercita): creado en 15 s, nodo HEALTHY;
    borrado limpio en motherbee y worker1 (sin directorio, sin entrada en el manifest, sin unit).
  - Lecturas entre hives (`nodes`, `versions`) en worker1, ingress1 y egress1: OK.
  - **0 rechazos del gate y 0 descartes por `routing.src`** en los 4 routers desde el deploy.
  - OPA: apply, apply y clear `converged` (10 / 6 / 4 s).
- **Rollback:** snapshot `pre-rule3-0-1-45` o `apt install fluxbee=0.1.44`.

## 0.1.44 — un nodo solo manda como sí mismo; fuera el aviso de rutas; OPA más rápido

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.43 · **Commit:** `005d0ad` (FINDINGS A-22,
  A-18; bitácora 2026-09-30)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Seguridad: el router descarta un frame cuyo `routing.src` no es el UUID con el que ese socket
    hizo HELLO. Antes un nodo que conociera el UUID de otro podía mandar como él (A-22).
  - Se borró el CONFIG_CHANGED que el admin mandaba después de cada alta/baja de ruta, VPN o tap
    (no lo usaba nadie) y el trato especial del router a CONFIG_CHANGED.
  - Después de un aviso de OPA, cada hive re-chequea su copia cada 1 s durante 30 s.
- **Build:** 17 min. **Publish:** ~6 min (44 paquetes). Snapshots `pre-src-binding-0-1-44` en las 4
  VMs (se borró antes `pre-opa-global-0-1-41`).
- **Install:** motherbee 03:03 UTC; spokes 03:05–03:08.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`; 0 nodos caídos.
  - Probe de suplantación en el motherbee: con el UUID del admin, ahora el router lo descarta
    (`routing.src is not the sending node`) y SY.config.routes no responde nada; con 0.1.43 respondía.
    Ningún descarte de tráfico legítimo en los 4 hives.
  - OPA: apply A, apply B, rollback y clear en ~5 s cada uno (antes 8–10 s), todos `converged`.
  - Rutas sin el aviso: una del motherbee aparece en la región LSA de worker1 a los ~11 s y se va a
    los ~7 s; una de worker1 llega al LSA del motherbee a los ~6 s, sin tocar la config del
    motherbee.
  - Estado final: sin policy de usuario ni rutas de prueba.
- **Rollback:** snapshot `pre-src-binding-0-1-44` o `apt install fluxbee=0.1.43`.

## 0.1.43 — el sync hint acepta una carpeta dist puntual (cierra el escaneo de la policy)

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.42 · **Commit:** `2b3abb2` (FINDINGS A-19)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:** el canal `dist` del sync hint acepta cualquier `fluxbee-dist*`; así anda el
  escaneo de `fluxbee-dist-policy` que pide el admin justo después de publicar una policy.
- **Build:** 9 min. **Publish:** ~6 min (43 paquetes). Snapshots `pre-sync-hint-0-1-43` en las 4 VMs
  (se borró antes `pre-purge-order-0-1-40`).
- **Install:** motherbee 01:43 UTC; esta vez se esperó a que el admin respondiera antes de los
  spokes (01:45–01:48).
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - apply A (v9) 8,4 s · apply B (v10) 10,5 s · rollback (vuelve a A) 8,4 s · clear 8,3 s: los
    cuatro `converged: true`, con la policy correcta en los 4 hives y sus routers. El motherbee
    escanea ~0,5 s después de aplicar; el resto es la transferencia y el chequeo de 5 s de cada hive.
  - config-routes: alta y baja de una ruta en worker1 (v12, v13) con el motherbee intacto (v8); el
    router de worker1 toma el cambio de su SHM al instante.
  - Estado final: ningún hive con policy de usuario.
- **Rollback:** snapshot `pre-sync-hint-0-1-43` o `apt install fluxbee=0.1.42`.

## 0.1.42 — OPA: escanear al publicar; solo el admin primario escribe

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.41 · **Commit:** `ac301d9` (FINDINGS
  A-19, A-20)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:** el admin le pide al orquestador del motherbee que escanee `fluxbee-dist-policy`
  antes del aviso; SY.opa.rules toma CONFIG_CHANGED solo de `SY.admin@motherbee`;
  SY.config.routes ya no aplica listas que lleguen por CONFIG_CHANGED; la respuesta de
  `POST /opa/policy` dice `compile_apply`.
- **Build:** 8 min. **Publish:** ~9 min (42 paquetes). Snapshots `pre-opa-fixes-0-1-42` en las 4 VMs
  (se borró antes `pre-identity-baseline-0-1-39`).
- **Install:** motherbee 01:16 UTC; spokes 01:18–01:21. Un primer core-update salió sin hash porque
  el admin todavía arrancaba: lo rechazaron, sin efecto.
- **Verificación en vivo:** binarios iguales en los 4 hives, 0 `failed`. **El escaneo no anduvo:** el
  orquestador rechaza `folder_id: fluxbee-dist-policy` (`INVALID_REQUEST`, solo acepta
  `fluxbee-dist`) y sin escaneo el watcher retuvo también el apply: a los 33 s solo el motherbee
  corría la v8, y el clear siguiente se fusionó con él en un único escaneo a los 64 s → se arregla
  en 0.1.43.
- **Rollback:** snapshot `pre-opa-fixes-0-1-42` o `apt install fluxbee=0.1.41`.

## 0.1.41 — una sola policy OPA de usuario en todos los hives, distribuida por Syncthing

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.40 · **Commit:** `72891be` (FINDINGS A-7;
  bitácora 2026-09-30; [`docs/opa-distribution.md`](../docs/opa-distribution.md))
- **Alcance:** motherbee + los tres spokes (core-update) + `hive.yaml` de motherbee, ingress1 y
  egress1 (SY.opa.rules en ingress/egress; receta en HANDBOOK §12).
- **Qué cambió:**
  - SY.opa.rules corre en los 4 hives. Solo el motherbee compila: aplica y publica el wasm en la
    carpeta Syncthing `fluxbee-dist-policy`; los demás hives lo instalan si el sha256 coincide.
  - Las escrituras OPA son globales (`/opa/policy*`); las de un hive puntual se rechazan. La
    respuesta dice qué hives ya la corren y cuáles siguen pendientes.
  - CONFIG_CHANGED viaja por el ruteo normal y cruza hives; se eliminaron
    `PUT /config/routes|vpns|taps`.
- **Build:** 17 min. **Publish:** ~9 min (`dpkg-scanpackages` sobre 41 paquetes). Snapshots
  `pre-opa-global-0-1-41` en las 4 VMs (se borró antes `pre-wf-mirror-0-1-38`).
- **Migración (00:25–00:39 UTC):** `hive.yaml` del motherbee editado antes del `apt install` (backup
  `hive.yaml.pre-opa-0-1-41`); el orquestador armó `dist/core/ingress` con 5 componentes y
  `dist/core/egress` con 4, y el binario llegó a ingress1/egress1 en menos de un minuto; después
  `hive.yaml` de esos dos spokes y core-update a los tres (TIMEOUT como siempre; los binarios
  quedaron con el mismo hash que en el motherbee).
- **Verificación en vivo:**
  - SY.opa.rules `running` en los 4 hives (en ingress1/egress1 la unit la creó el orquestador al
    arrancar). Carpeta `fluxbee-dist-policy`: `sendonly` con 4 devices en el motherbee (heredados
    de vendor) y `receiveonly` con 2 en cada spoke. 0 servicios `failed`.
  - `POST /opa/policy` (v6): `converged: true` en 16 s; los 4 routers con v6 cargada.
  - Ponerse al día: con Syncthing de egress1 cortado (nft temporal en :22000), la v7 respondió
    `pending: [egress1]` y egress1 siguió con su v6; al desbloquear instaló la v7 solo, en 27 s.
  - Clear: el motherbee limpió al instante, pero los spokes tardaron ~63 s (el watcher de Syncthing
    retiene 60 s un cambio hecho solo de renames y borrados) y la respuesta salió con los tres
    pendientes → se arregla en 0.1.42.
  - Escritura para un hive puntual → 400; ruta vieja `/hives/{h}/opa/policy/clear` → 404. En
    ingress1/egress1: `/var/lib/fluxbee/opa` `700`, SHM `600`.
  - Factory reset en dry-run: ahora lista la policy de los 4 hives.
- **Rollback:** snapshot `pre-opa-global-0-1-41`, o `apt install fluxbee=0.1.40` + restaurar
  `hive.yaml.pre-opa-0-1-41` en motherbee, ingress1 y egress1.
  - **Nota (2026-10-01, panel DTAP P-9):** antes del `apt install`, `POST /opa/policy/clear` y
    esperar `converged: true`; si no, la última policy global queda aplicada en los workers y en
    memoria en los routers de ingress/egress. Ver HANDBOOK §12, "El rollback".

## 0.1.40 — purga de WF en el motherbee en orden y tolerante a "ya no existe"

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.39 · **Commit:** `43dd174` (FINDINGS A-15)
- **Qué cambió:** en un worker, wf-rules purga en el motherbee dejando la versión current al final
  (el admin no deja borrarla mientras haya otras) y toma `RUNTIME_NOT_FOUND` /
  `RUNTIME_VERSION_NOT_FOUND` como hecho.
- **Build:** 7 min. **Install:** motherbee 23:03 UTC; spokes 23:05 (solo cambió wf-rules).
- **Verificación en vivo — ciclo completo de un WF en worker1:**
  - crear → nodo HEALTHY en 0.0.1 (18 s: publica, espera la sync, spawnea);
  - re-aplicar con cambio → HEALTHY en 0.0.2 (18 s);
  - borrar → OK; el motherbee queda sin versiones ni entrada en el manifest; el espejo de worker1
    con 0 cambios locales (19/19).
  - Limpieza: `AI.vaultprobe@worker1` borrado con su ILK purgado (el primario vuelve a 24 ILKs,
    como al empezar el día); sin nodos ni scripts de prueba en las VMs.
- **Rollback:** snapshot `pre-purge-order-0-1-40` o `apt install fluxbee=0.1.39`.

## 0.1.39 — identity no borra los ILK de un hive que arrancó sin primario; purga de WF dirigida

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.38 · **Commits:** `2248612`, `85c3244`
  (FINDINGS A-15, A-17). Un primer build de 0.1.39 sin `85c3244` se rehízo antes de publicarse.
- **Qué cambió:** la réplica de identity no publica su conjunto de ILK propios hasta tener una base
  del primario; wf-rules dirige `remove_runtime_version` al motherbee (`target`).
- **Install:** motherbee 22:38 UTC; spokes 22:41 (solo se reiniciaron identity y wf-rules, que eran
  los binarios cambiados).
- **Verificación en vivo:**
  - Réplica de worker1 arrancada con el primario bloqueado (nft temporal): logueó
    `holding its self-owned snapshot` mientras estuvo sin base; al volver, snapshot de 25 ILKs. El
    primario **siguió en 25, con el ILK de `AI.vaultprobe@worker1`** (con 0.1.38 había pasado de
    25 a 24).
  - Workflow en worker1: creado en 3 s, nodo HEALTHY. Delete: el motherbee borró la 0.0.1 y la
    sync limpió worker1 sin cambios locales, pero la purga cortó en la 0.0.2 (`RUNTIME_NOT_FOUND`:
    directorio huérfano) → se arregla en 0.1.40.
- **Rollback:** snapshot `pre-identity-baseline-0-1-39` o `apt install fluxbee=0.1.38`.

## 0.1.38 — wf-rules no toca el espejo `dist/` de un worker; el orquestador no spawnea sin archivos

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.37 · **Commits:** `676732d`, `20baec3`
  (FINDINGS A-15)
- **Qué cambió:** en un hive que no es el motherbee, wf-rules purga pidiéndole
  `remove_runtime_version` al admin en vez de borrar su espejo; el orquestador responde
  `RUNTIME_NOT_PRESENT` hasta que el `package.json` del paquete está en el hive.
- **Build:** 4 min (incremental). **Install:** motherbee 22:07 UTC; spokes 22:10–22:12.
- **Reparación puntual:** espejo `dist-runtimes` de worker1 restaurado con `POST /rest/db/revert`
  de Syncthing (15 → 0 cambios locales). Después `WF.w1probe@worker1` quedó HEALTHY: **el primer WF
  corriendo en un worker**.
- **Verificación en vivo:** el delete dejó el espejo limpio (0 cambios locales), pero el pedido al
  admin salía sin `target` (`INVALID_REQUEST`) → se arregla en 0.1.39.
- **Rollback:** snapshot `pre-wf-mirror-0-1-38` o `apt install fluxbee=0.1.37`.

## 0.1.37 — lo que salió de validar 0.1.36: spawn de WF en workers, veredicto del vault, permisos de OPA

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.36 · **Commits:** `3055a40`, `6c708a6`,
  `c09f9af` (FINDINGS A-12 a A-14)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - wf-rules espera (hasta 20 s) a que el paquete recién publicado llegue al hive antes de spawnear.
  - SDK: `VaultClient::get`/`list` conservan el veredicto del vault (`KEY_NOT_FOUND` ya no llega
    como error JSON).
  - SY.opa.rules: SHM `0600` (era `0666`), policy `0600`, dirs `0700`.
- **Build:** VM110 `build-deb.sh 0.1.37` (22 min) → publish (37 paquetes). Snapshots
  `pre-followups-0-1-37` en las 4 VMs (se borró antes el más viejo de cada una).
- **Install:** motherbee 21:37 UTC; spokes 21:40–21:42 (TIMEOUT como siempre; `rt-gateway` con el
  mismo hash en los 4 hives).
- **Verificación en vivo:**
  - `/dev/shm/jsr-opa-motherbee` y `…-worker1` → `rw-------`; `/var/lib/fluxbee/opa` → `700`.
  - Workflow en worker1: `RUNTIME_NOT_AVAILABLE` a las 21:43:39/40/42/46 y spawn OK a las 21:43:51,
    cuando llegó el manifest. **El nodo quedó en crash-loop** por archivos del paquete faltantes:
    wf-rules los había borrado del espejo local (A-15) → se arregla en 0.1.38.
- **Rollback:** snapshot `pre-followups-0-1-37` o `apt install fluxbee=0.1.36` + core-update.

## 0.1.36 — config con alcance de hive, control de origen, vault/admin desde workers, identity se pone al día

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.35 · **Commits:** `ad48ecb`, `15d53cc`,
  `d24e3aa`
- **Alcance:** **motherbee + los tres spokes** (core-update a worker1, ingress1 y egress1: cambian
  el router, SY.config.routes, identity, cognition y los nodos Go).
- **Qué cambió** (bitácora 2026-09-30; FINDINGS A-8 a A-11). Los cuatro bugs se confirmaron en
  vivo con 0.1.35 antes de tocar nada:
  - `ConfigChangedPayload.hive`: un `add_route` en worker1 ya no pisa las rutas del motherbee, y
    una operación OPA dirigida a worker1 ya no se ejecuta en el motherbee.
  - CONFIG_CHANGED pasa a ser acción protegida (gate en el router). SY.config.routes y
    SY.opa.rules solo aceptan mutaciones y comandos de `SY.admin@motherbee`.
  - `VaultClient::for_primary` y wf-rules → `SY.admin@motherbee`: AI, cognition, IO y WF en un
    worker llegan al vault y al admin.
  - La réplica de identity se resincroniza con un snapshot en cada suscripción.
- **Build:** VM110 `build-deb.sh 0.1.36` (22 min: cambió el SDK) → publish (36 paquetes).
  Snapshots `pre-config-scope-0-1-36` en las 4 VMs (la más vieja de VM100 se borró antes).
- **Install:** motherbee 20:42→20:43 UTC; core-update de los spokes → `TIMEOUT` como siempre, pero
  aplicado: `rt-gateway` con el mismo hash en los 4 hives y servicios reiniciados 20:44–20:46.
- **Verificación en vivo** (los mismos pasos que confirmaron cada bug):
  - Dos altas en worker1 → el motherbee sigue en 0 rutas; su SY.config.routes loguea
    `config changed addressed to another hive; ignoring`.
  - Nodo falso: CONFIG_CHANGED → `router dropped CONFIG_CHANGED from unauthorized origin`;
    `add_route` directo → `FORBIDDEN`; `rollback_policy` → `UNAUTHORIZED`. Rutas intactas.
  - `node_config` legítimo del orquestador en worker1 → pasa el gate.
  - AI y cognition en worker1 consultan `SY.vault@motherbee` (antes `NODE_NOT_FOUND` contra
    `SY.vault@worker1`).
  - Workflow en worker1 → **publica** (antes `PACKAGE_PUBLISH_FAILED: UNREACHABLE`). El primer
    spawn todavía choca con la sincronización de Syncthing (~11 s): se arregla en 0.1.37.
  - Réplica de worker1 arrancada con el primario inalcanzable (regla nft temporal) → DEGRADED →
    al volver la conexión, `identity full sync applied (delta stream subscribed)` sola.
  - Clear de OPA dirigido a worker1 → el motherbee lo ignora; el admin informa `pending worker1`
    (que llegue a los spokes es el diseño pendiente, A-7).
- **Rollback:** snapshot `pre-config-scope-0-1-36` (las 4 VMs) o `apt install fluxbee=0.1.35` +
  core-update de los spokes.

## 0.1.35 — admin: los orquestadores vuelven a poder marcar el ILK de un nodo (fix de 0.1.34)

- **Fecha:** 2026-09-28 (ART) · **Versión anterior:** 0.1.34 · **Commit:** `5f1cd71`
- **Alcance:** **motherbee** (VM100). Solo cambian SY.admin y el script de reset; los spokes siguen
  con el core de 0.1.34 (el admin no corre en ellos).
- **Qué cambió:** la **regresión de 0.1.34**. Al sumar `delete_ilk` a la superficie de Cloud, el
  gate de relay (EDGE-06) empezó a rechazar el teardown del **orquestador**. El kill con
  `purge_instance` respondía ok pero dejaba el ILK del nodo **activo y huérfano**. Fix: cualquier
  `SY.orchestrator@*` puede usar `delete_ilk` (y solo eso), siguiendo la regla hermana de
  `vault_put`. Además, el reset verifica OPA por resultado, hive por hive.
- **Build:** VM110 `build-deb.sh 0.1.35` (7 min) → publish (35 paquetes).
- **Verificación en vivo:**
  - e2e 27/31. Los 4 FAIL son hallazgos previos y explicados: la réplica rechaza el listado del
    admin y la escritura de OPA no llega a los spokes.
  - Se verificó el kill+purge: el ILK del nodo queda purgado y el nombre libre.
  - **Reset instalado por el `.deb`** con datos reales: 1 nodo, 2 ILKs, 2 secretos, **8 tenants**
    y la política OPA v5, con RC=0 y 0 problemas. El segundo dry-run da vacío.
  - Puerta de Cloud en 200, con 13 acciones.
- **Rollback:** snapshot `pre-teardown-fix-0-1-35` (VM100) o `apt install fluxbee=0.1.34`, que
  re-introduce la regresión.

## 0.1.34 — identity: marcar / restaurar / purgar · `opa_clear` · reset a fábrica completo

- **Fecha:** 2026-09-28 (ART) · **Versión anterior:** 0.1.33 · **Commits:** `13f426f`, `60331c5`
  (más `f47dcb2` y `c1fee0d` del reset).
- **Alcance:** **motherbee + los 3 spokes**, con core update, porque cambian el router,
  SY.identity y SY.opa.rules, que también corren en los spokes.
- **Qué cambió:**
  - **Identity:** borrar un ILK o un tenant solo **marca**. La marca es reversible, oculta el
    registro y deja sus claves reservadas; la **purga** es física y exige la marca. El tenant
    marca en cascada a sus ILKs.
  - **Migración de DB v2.**
  - **Cloud:** marca y restaura, siempre acotado al tenant que reclama; **no puede purgar**.
  - **`opa_clear`:** los routers descargan la política de usuario (antes la ignoraban hasta
    reiniciarse).
  - **`fluxbee-factory-reset`:** viaja en el `.deb`.
- **Build:** VM110 `build-deb.sh 0.1.34` (24 min) → publish (34 paquetes). Snapshots
  `pre-lifecycle-0-1-34` en las 4 VMs.
- **Verificación en vivo:**
  - la migración v2 se aplicó;
  - los spokes hicieron el core update (hash `e67a8815…`) y sus servicios reiniciaron;
  - e2e 26/30 por la puerta pública real: marcar, ocultar, reservar, scope por tenant, cascada,
    restaurar y purgar;
  - el router descargó la política OPA (visto en su log).
- **Regresión encontrada en el e2e:** el teardown del orquestador quedó bloqueado. Se corrigió en
  0.1.35.
- **Rollback:** snapshot `pre-lifecycle-0-1-34` (las 4 VMs) o `apt install fluxbee=0.1.33`. Ojo:
  la migración v2 solo **agrega** columnas, así que 0.1.33 las ignora sin problema.

## 0.1.33 — io.cloud: get_ilk email-SOLO (cross-tenant) — el login del website

- **Fecha:** 2026-08-28 (ART) · **Versión anterior:** 0.1.32 · **Commit:** `01b6266`
- **Alcance:** **motherbee** (VM100). io.cloud + fluxbee_sdk (warn seqlock en el reader de listado) + docs.
- **Qué cambió:** el website (OAuth Google) tiene SOLO el email en el primer login; `params.tenant_id` ahora es
  OPCIONAL con `params.email`. Con tenant: probe O(1) 0.1.32 sin cambios. SIN tenant: scan cross-tenant de los
  canales `cloud` (`list_ich_options_from_hive_id`, propaga errores) → `{exists, ilk: solo si match único,
  matches:[UN subset POR tenant donde existe el email — cada uno con su tenant_id]}`. De `matches` el website
  saca el tenant; 0 matches → create_tenant+register_human; N matches → selector de empresa.
- **Hardening de la review (2 MEDIUM pre-ship):** (1) `tenant_id` PRESENTE pero malformado (número, vacío,
  formato inválido) = error fuerte — nunca ensancha silenciosamente un probe con tenant a scan global (branch
  por presencia de key, no por parse); (2) `matches` garantiza uno-por-tenant: estados transitorios de identity
  (ventana de merge-alias, address takeover) pueden dejar 2 ilks activos en un mismo (cloud,email,tenant) — los
  tenants ambiguos se re-prueban con el resolver O(1) autoritativo (misma respuesta que el probe con tenant).
  + LOW: warn en seqlock-timeout del reader de listado (outages de first-login diagnósticables).
- **Semántica aceptada (documentada):** canales disabled matchean (register_human los crea enabled:false — un
  filtro estricto rompería el first-login); el scan es O(hive) por request (aceptado al tamaño actual; list_ilks
  ya escanea igual). Oráculo email→tenants expuesto SOLO a la clase de caller ya confiada (bearer del edge,
  default-deny sin edge configurado).
- **Build:** VM110 `build-deb.sh 0.1.33` → publish repo :8900 (33 paquetes).
- **Verificación E2E (7/7 desde internet):** login 1-empresa (`ilk` poblado con tenant), register en 2ª empresa,
  login 2-empresas (`ilk:null, matches:[2]` uno por tenant), email desconocido (`exists:false`), `tenant_id:42`
  → error fuerte (no scan), y regresiones email+tenant / ilk_id intactas.
- **Rollback:** `apt install fluxbee=0.1.32` (aditivo; los selectores 0.1.32 no cambiaron).

## 0.1.32 — io.cloud: selector por EMAIL en get_ilk / get_ilk_details

- **Fecha:** 2026-08-27 (ART) · **Versión anterior:** 0.1.31 · **Commit:** `90fd64a` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `io.cloud` + `fluxbee_sdk` (identity readers + catálogo cloud) + docs.
- **Qué cambió:** el Cloud tiene el email del humano (login Google) pero no el `ilk_id`; ambos reads aceptan
  `params.email` + `params.tenant_id` como selector alternativo a `params.ilk_id` (exactly-one, en ambos ops).
  El email ES la dirección del canal `cloud` del ilk (register_human lo provisiona así) → el `get_ilk` local
  resuelve con un probe O(1) del índice SHM `(canal, dirección, tenant)` — patrón io.api inbound; más barato que
  el propio path por id (O(n)). `get_ilk_details` pre-resuelve email→ilk_id local y relaya el `get_ilk` de admin
  sin cambios (admin/identity no ganan selector email; `ILK_NOT_FOUND` sin tocar admin si no matchea).
  `tenant_id` OBLIGATORIO con email (unicidad por `(canal, dirección, tenant)` — el mismo email puede ser dos
  ilks en dos empresas) y validado canónico `tnt:<uuid>` fail-loud. SDK: variantes
  `resolve_identity_option_*_strict` (SHM ilegible = `Err`, nunca miss silencioso — el laundering F-09 es para
  el degrade de io.api, no para un read API autoritativo).
- **Review adversarial (2 lentes) pre-ship:** ambos MEDIUM corregidos ANTES del deploy (semántica strict de SHM;
  exactly-one en el op con PII); oráculo cross-tenant descartado (el tenant integra la key del índice); el LOW de
  request_id era falso positivo (el wrapper del run_loop lo inyecta). Tests: SDK 6/6+6/6, io-cloud 9/9, admin 3/3.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.32` → 245 MB · publish repo :8900 (32 paquetes).
- **Deploy:** reboot (qga caído, patrón conocido) → `apt install fluxbee=0.1.32` → el restart manual de io.cloud
  llegó antes de que el unit existiera, y **el liveness-reconcile de 0.1.30 lo respawneó solo con el binario
  nuevo** (~60s) — el fix anterior auto-desplegó este feature.
- **Verificación E2E (10/10 desde internet):** hit por email (`exists:true` + subset), case-insensitive, miss,
  tenant equivocado → `exists:false` (aislamiento), exactly-one en ambos ops, email-sin-tenant error claro,
  details por email → ficha completa, `ILK_NOT_FOUND` con request_id, y regresión por `ilk_id` OK.
- **Rollback:** `apt install fluxbee=0.1.31` (feature aditivo; el selector por id no cambió).

## 0.1.31 — SDK/rpc: fix del veneno del edge (response_only familia-completa) — el bug real de crearPersona

- **Fecha:** 2026-08-26 (ART) · **Versión anterior:** 0.1.30 · **Commit:** `862c5e4` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `crates/fluxbee_sdk/src/rpc.rs` — linkeado estático en TODOS los binarios (rebuild integral del .deb; hicieron falta reboot para respawnear los runtimes con el SDK nuevo).
- **El bug (prod, determinístico):** `register_human` en io.cloud espera el veredicto del frontdesk con
  `send_with_matcher(frontdesk_reply_matcher())` cuyo success matcher es `any_msg_type(user)` (inevitable: el
  veredicto es un frame `user` con `meta.msg=None`). `send_with_matcher` registraba los success shapes en el
  registro **PERMANENTE** `response_only` (solo-insert, se arma AL ENVIAR). `classify` consulta ese registro por
  FORMA (sin trace) ANTES del catch-all `Any→Command` de io.cloud → tras UN `register_human` (éxito O timeout),
  TODO frame user-kind entrante (todos los Cloud requests del edge) se descartaba como "orphaned response" en
  silencio (debug-level) → edge 504 → Cloud "crearPersona falló (FRONTDESK_REJECTED)". Solo un restart curaba.
  **Probado por 3 vías:** código, panel de 4 mappers, y experimento controlado en prod (baseline 200×3 →
  1 register SUCCESS → probe 6s después timeout permanente). Refuta el framing "presence decay ~6min":
  monitor 12min solo-lists = 36/36 OK; VM100 sana en pleno fallo (cpu 0%, mem 15%) — NO era Proxmox/memoria.
- **El fix:** `register_response_only` saltea `AnyMsgOfType` (misma razón que el skip de `Any`: global drops).
  Exact/OneOf siguen registrándose — protecciones AF-P2b del orquestador intactas. Late replies familia-completa
  quedan cubiertas por pending (trace) + stale table (30s TTL) + gates propios de cada nodo (io.cloud ignora
  src≠edge fail-closed). **Desactiva 3 bombas: io.cloud (detonada), io.api (handoff idéntico) y
  `sy_admin::send_admin_request` (`any_msg_type(admin)`).** Helper muerto removido; test venenoso invertido en
  regresión `family_wide_success_matcher_never_poisons_command_traffic`; doc RPC multiplexing actualizado.
- **Review:** 39/39 tests SDK verdes · adversarial 3 lentes (blast-radius de todos los callers, ventanas de
  protección, coherencia de tests): **0 HIGH / 0 MEDIUM**, solo LOW de corrimiento de métricas/logs.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.31` → 234 MB. **Publish:** `apt-repo-publish.sh` → repo :8900
  (ojo: el server `:8900` es un unit transient `systemd-run` que NO sobrevive reboots de VM110 — hubo que relevantarlo).
- **Verificación en vivo (E2E desde internet, post-reboot):** **4 `register_human` consecutivos** (3 frescos +
  1 repetido idempotente, incluido el flujo real del operador) todos `status=ok success=True reg=complete`, con
  `list_cloud_actions` 200 intercalado tras CADA uno + estabilidad 200×3 — con 0.1.30 el primer register mataba
  todo el tráfico posterior del edge.
- **Rollback:** snapshot `pre-rpc-poison-fix-0-1-31` (VM100) o `apt install fluxbee=0.1.30` + reboot (re-introduce el veneno).

## 0.1.30 — orquestador: self-heal de runtimes managed muertos + adiós `is_packaged_singleton`

- **Fecha:** 2026-08-26 (ART) · **Versión anterior:** 0.1.29 · **Commit:** `af1166a` (branch `daily_onworking_coa`)
  (+ `9e81687` reword docs "singleton"→"managed runtime").
- **Alcance:** **motherbee** (VM100). Toca `sy_orchestrator` + `fluxbee_sdk::managed_node`.
- **El bug (prod):** `IO.cloud@motherbee` fue hallado MUERTO (unit transient inactive) con la MB 19.8 días up y
  nada lo resucitaba: los runtimes managed corren con `systemd-run --property Restart=always`, pero si agotan el
  start-limit nadie los recrea — `reconcile_persisted_custom_nodes` corría SOLO en el bootstrap, y el watchdog de
  5s solo los LOGUEABA ("node disconnected") mientras SÍ reinicia SY.*/rt-gateway → edge 504 hasta el próximo boot.
- **El fix:** `run_managed_runtime_reconcile_loop` — task PROPIA de 60s (NO el hot-path del watchdog de 5s: un
  systemctl lento jamás frena el self-heal de SY.*) que re-corre el reconcile persistido; el reconcile ahora hace
  `try_lock_node` (SO-04) por nodo — si un admin-op tiene el lock, saltea y reintenta el ciclo siguiente. Solo
  revive nodos `relaunch_on_boot=true`. **Nota operativa:** `kill_node` sin purge de un nodo boot ahora revive en
  ≤60s (consistente con SY.*; bajar durable = purge). Además: borrado el mecanismo muerto
  `is_packaged_singleton`/`PACKAGED_SINGLETON_NODES` (lista vacía; el guard de doble-dueño lo sostiene `kind=="SY"`/RT.gateway).
- **Review:** adversarial 3 lentes — el hallazgo mayor (reconcile inline bloqueaba el watchdog) se corrigió
  ANTES del deploy (task propia + try_lock).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.30` → 234 MB. **Publish:** repo :8900.
- **Verificación en vivo:** boot-reconcile relanzó los 9 runtimes (`started=9 skipped=0 failed=0`); el loop
  periódico corre cada 60s exacto (`reconcile completed started=0 skipped=9`); endpoint Cloud 200 post-boot.
- **Rollback:** snapshot `pre-liveness-fix-0-1-30` (VM100) o `apt install fluxbee=0.1.29`.

## 0.1.29 — io.cloud: camino de LECTURA de Cloud (fase 2) — reads rápidos SHM + get_ilk_details relay

- **Fecha:** 2026-08-25 (ART) · **Versión anterior:** 0.1.28 · **Commit:** `a0bc25d` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `fluxbee_sdk::cloud` + el nodo runtime `IO.cloud` + el nodo core `SY.admin`.
- **Qué cambió:** le da a Fluxbee Cloud una forma de **leer** lo que creó (patrón io.api: existencia/subset desde la
  SHM local sin round-trip; data completa por relay a admin).
  - **SDK (single source):** `CLOUD_LOCAL_OPS += get_ilk/get_tenant/list_ilks` (reads SHM que io.cloud sirve solo);
    `CLOUD_OP_ACTIONS += get_ilk_details → get_ilk` (relay). `CLOUD_EXPOSED_ACTIONS` gana `get_ilk` → admin
    `authorize_cloud_relay` + el catálogo Cloud lo auto-permiten/publican (advertised==enforced). `cloud_action_catalog` documenta las 4.
  - **io.cloud:** `handle_shm_read` lee la SHM de identidad vía `fluxbee_sdk::identity` (`list_ilks_from_hive_id`/
    `tenant_exists_in_hive_id`, por `config.hive_id`) — los mismos readers de io.api, **sin round-trip**. Subset
    `{ilk_id, ilk_type, registration_status, tenant_id, display_name}`. `get_ilk_details` → admin `get_ilk` (identification PII + canales + tenant).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.29` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo (E2E desde ingress):** las **4 ops OK** — `get_ilk` `{exists,ilk:subset}`; `get_tenant`
  `{exists,ilk_count:4}`; `list_ilks` `{count:4, ilks:[…pepito…]}`; `get_ilk_details` registro completo
  (`identification{email,phone,company,attributes}` + `channels` + `tenant{pepito}`). SDK 5/0 · io.cloud 9/0 ·
  admin catalog test actualizado. **0 units failed**, io.cloud `NRestarts=0` (un 502 transitorio inicial por el ICH
  re-registrándose tras el restart). **Contrato para Cloud dev en `docs/io-cloud-api.md` §4.6–4.9.**
- **Nota (owner-deferred):** ownership de tenant sigue MVP-trusted — un holder del bearer puede leer cualquier id
  que nombre (misma postura que las escrituras).
- **Rollback:** snapshot VM100 `pre-cloud-readpath-0-1-29`, o `apt-get install -y --allow-downgrades fluxbee=0.1.28`.

---

## 0.1.28 — frontdesk: reconciliar CONFIG_GET/CONFIG_SET con el modelo autónomo (Model D', como architect)

- **Fecha:** 2026-08-25 (ART) · **Versión anterior:** 0.1.27 · **Commit:** `1438737` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `SY.frontdesk.gov` (`ai_node_runner`).
- **Qué cambió:** un panel de 4 agentes confirmó que architect Y el executor de admin son autónomos **y
  MANTIENEN** un CONFIG_GET/CONFIG_SET reconciliado — así que conformar = **reconciliar, no borrar** el plano.
  - **`refresh_ai_gate()`**: el seam único de resolve-token-y-setear-gate (análogo a `refresh_architect_ai_runtime`),
    ahora usado por `boot_self_configure`, `handle_vault_secret_changed` **y** CONFIG_SET.
  - **CONFIG_SET (Fork B, owner-confirmado):** era inyector de config; ahora modelo architect — **rechaza TODO**
    campo config/secret/behavior (`frontdesk_rejected_config_field`) y es un **disparador de re-resolve del token
    del vault**; no persiste nada.
  - **CONFIG_GET reconciliado:** `ok` ahora refleja el gate del token (Configured), no la presencia de config;
    `required_fields`/`optional_fields` → `[]`; agrega `config.ai {default_provider, model}` (hive-wide); renombra
    `secrets[]` → `resources[]` (+required) espejando architect; notas autónomas.
  - Removidas las 5 huérfanas de inyección (`ok_response`, `persist_dynamic_config`, `write_json_atomic`,
    `first_secret_bearing_config_field`, `parse_effective_config_doc`; su test re-apuntado). 27/0 tests.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.28` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `0.1.28`; **boot autónomo sigue OK** (degraded → VAULT_SECRET_CHANGED → Configured);
  **regresión handoff determinista** → HTTP 200 `success:true complete`; **0 units failed**. (Nota: la ruta operador
  `GET /nodes/.../config` da `NODE_CONFIG_NOT_FOUND` porque lee un ARCHIVO de config persistido que el nodo autónomo
  ya no tiene — igual que architect; el CONFIG_GET vivo se sirve por el canal mesh, verificado en código + tests.)
- **DIFERIDO (pasada de limpieza catalogada, marcado por el panel):** la ruta `--config`/`run_one_config` (YAML,
  MUERTA en prod — el unit systemd no pasa `--config`) + sus helpers + los tipos de input YAML + los pre-existentes
  `with_jitter`/`parse`.
- **Rollback:** snapshot VM100 `pre-frontdesk-configplane-0-1-28`, o `apt-get install -y --allow-downgrades fluxbee=0.1.27`.

---

## 0.1.27 — frontdesk: bootstrap AUTÓNOMO (resuelve el token del vault al boot + actúa en el broadcast)

- **Fecha:** 2026-08-25 (ART) · **Versión anterior:** 0.1.26 · **Commit:** `000de90` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `SY.frontdesk.gov` (`ai_node_runner`).
- **Qué cambió:** el frontdesk se conforma al patrón canónico de nodo system SY.* (verificado con un panel contra
  `SY.architect` `build_architect_ai_runtime`/`refresh_architect_ai_runtime` y el executor de `SY.admin` — mismo
  mecanismo, mismo seam). Es un nodo **autónomo**: su runtime AI es **baked** (siempre ai_chat — prompt
  `frontdesk_default_instructions` + engine `load_hive_ai_engine` hive.yaml/fallback), y el **único input externo
  es el token del vault**.
  - **Boot:** arma el behavior baked, `boot_self_configure` resuelve el token (`resolve_ai_api_key`, Model D' root)
    → `Configured` si está, sino `Unconfigured` (degradado — el handoff determinista igual anda). Build baked
    falla (hive.yaml roto) → `FAILED_CONFIG`.
  - **Broadcast:** `handle_vault_secret_changed` era un no-op probe+log (EL bug — nunca cambiaba estado); ahora
    **actúa** como `refresh_architect_ai_runtime` — re-resuelve y setea `Configured`/`Unconfigured` en vivo.
  - Reusa solo seams existentes (sin builder ni vault-path nuevos). 27/0 tests.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.27` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `0.1.27` instalado; **el frontdesk pasó a `Configured` desde el token del vault** en el
  arranque (boot degradado 00:52:19 → `VAULT_SECRET_CHANGED` op=put openai_api_key 00:52:20 → **Configured, LLM
  path live**) — sin CONFIG_SET. **Regresión handoff determinista OK** (register_human desde ingress → HTTP 200
  `success:true registration_status:complete`; un 502 transitorio inicial por io.cloud re-registrando su ICH tras
  el restart). frontdesk + io.cloud `active`, `NRestarts=0`, **0 units failed**.
- **DIFERIDO (EDIT 3, owner-confirmado, a pasada de limpieza catalogada):** sacar el plano CONFIG_SET/spawn
  (`apply_config_set` + `load_persisted_dynamic_config` en AMBAS rutas de boot + ~6 helpers) — cascada enredada;
  por la regla "no borrar a ciegas" queda para una pasada dedicada. `apply_config_set` sigue funcional (nadie le
  manda CONFIG_SET al frontdesk en la práctica). Warnings pre-existentes `with_jitter`/`parse` también a esa pasada.
- **Rollback:** snapshot VM100 `pre-frontdesk-autonomous-0-1-27`, o `apt-get install -y --allow-downgrades fluxbee=0.1.26`.

---

## 0.1.26 — frontdesk: el handoff determinista (JSON) corre sin el gate de Configured/LLM (fix register_human)

- **Fecha:** 2026-08-24 (ART) · **Versión anterior:** 0.1.25 · **Commit:** `cb2d192` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `SY.frontdesk.gov` (`ai_node_runner`).
- **Root cause (hallado con la observabilidad de 0.1.25):** `register_human` nunca completaba porque el frontdesk
  **está UNCONFIGURED en todos los boots** (nunca fue seedeado ni recibió CONFIG_SET) y `on_message` rechazaba
  **TODO** mensaje `user` con `node_not_configured` **antes** de mirar el payload → io.cloud lo veía como
  `FRONTDESK_REJECTED` → el ilk quedaba `temporary`. (Confirmado por la línea `Cloud op completed error_code=FRONTDESK_REJECTED` + el frontdesk sin logs de handoff.)
- **Qué cambió:** el alta de humano tiene DOS métodos distinguidos por el **método**, no por ser humano: **auto**
  (llega un JSON `frontdesk_handoff` → determinista, ILK_REGISTER, sin LLM) y **conversacional** (el humano charla
  y el LLM junta los datos). El método determinista se maneja **antes** del gate de Configured → `register_human`
  registra aunque el frontdesk no tenga LLM. El path conversacional/LLM sigue requiriendo Configured.
  `handle_frontdesk_handoff` es config-independiente. Test nuevo `frontdesk_handoff_runs_deterministically_even_when_unconfigured`.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.26` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.26` instalado; **frontdesk reinició (18:40) + `active`**, io.cloud `active`
  `NRestarts=0`, **0 units `failed`**. Pre-deploy: sy-frontdesk-gov 27/0 tests verdes. **Pendiente:** E2E del
  register_human desde la pantalla (debería registrar ahora → ilk `complete`).
- **Tema 2 (separado):** el secret openai NO se perdió; el frontdesk está UNCONFIGURED sólo porque nunca fue
  configurado (peer AI.chat tiene config v2). El path conversacional/LLM necesita configurar el frontdesk — aparte.
- **Rollback:** snapshot VM100 `pre-frontdesk-handoff-0-1-26`, o `apt-get install -y --allow-downgrades fluxbee=0.1.25`.

---

## 0.1.25 — observabilidad: outcome de cada op de Cloud + veredicto del frontdesk (fase 1 consolidación tenant/ILK)

- **Fecha:** 2026-08-24 (ART) · **Versión anterior:** 0.1.24 · **Commit:** `69b1c46` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo runtime `IO.cloud` + el nodo core `SY.frontdesk.gov` (`ai_node_runner`). **Solo logging, cero cambio de comportamiento.**
- **Qué cambió (impacto operativo):** hacer visible el round-trip para diagnosticar por qué un `register_human` no completa (hoy el frontdesk procesa+responde pero no logea nada a INFO → el veredicto es invisible).
  - **io.cloud:** la línea de **egreso/outcome** que faltaba — cada op de Cloud logea `{status, error_code, registration_status, ilk_id, tenant_id, elapsed_ms}` a INFO, con el **mismo `trace_id`** que el ingreso → el round-trip completo edge→io.cloud→admin/identity/frontdesk queda greppable por `trace_id`. (Detalle: dentro de los macros `tracing::*` un `Value` pelado resuelve a `tracing::Value`, así que las lecturas serde usan closures.)
  - **frontdesk (Gov):** promover a INFO/WARN la **decisión del handoff** en cada salida: parseó-como-handoff vs cayó-a-conversacional (**WARN** cuando un payload con forma de handoff NO parsea — la falla silenciosa que estamos cazando), operación no soportada, incompleto→needs_input (ilk NO registrado), REGISTERED (complete), o register FAILED.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.25` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.25` instalado; **io.cloud + sy-frontdesk-gov + sy-admin + sy-identity `active`**, io.cloud `NRestarts=0`, **0 units `failed`**. Pre-deploy: io-cloud 8/0 + sy-frontdesk-gov 26/0 tests verdes.
- **Siguiente:** reproducir `register_human` desde la pantalla de Cloud → los nuevos logs muestran EN VIVO por qué no completa (parseo del handoff o campos faltantes) → fix al source. Después, fase 2 = lecturas (SHM E/!E + `get_ilk_details` relay).
- **Rollback:** snapshot VM100 `pre-observability-0-1-25`, o `apt-get install -y --allow-downgrades fluxbee=0.1.24`.

---

## 0.1.24 — seguridad: reservar los namespaces de infra del vault frente al relay `put_token` de Cloud

- **Fecha:** 2026-08-23 (ART) · **Versión anterior:** 0.1.23 · **Commit:** `35349ef` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `fluxbee_sdk::vault` + el nodo core `SY.admin` (systemd) + el nodo runtime `IO.cloud`.
- **Qué cambió (impacto operativo):** cierra el **MEDIUM** de la auditoría de superficie externa de io.cloud.
  `put_token`→`vault_put` no tenía guarda de namespace de key: un relay de Cloud semi-confiable (o comprometido)
  podía **sobrescribir cualquier key** del vault por charset — incluido `edge_channel_secret:<ich>` (el bearer
  que protege un endpoint externalizado, el suyo propio incluido), `edge_tls`, o `ssh:<hive_id>` (la recovery key
  del spoke). Eso es una superficie de **DoS/takeover**, no "guardar un token de provider".
  - Fix single-source + defense-in-depth: `fluxbee_sdk::vault::CLOUD_RESERVED_VAULT_KEY_PREFIXES`
    (`edge_channel_secret:`, `edge_tls`, `ssh:`) + `is_cloud_reserved_vault_key`. Las keys de peer-auth
    (mesh-HMAC / WAN-mTLS) viven en el **filesystem**, no en el vault, así que ya estaban fuera de alcance.
  - Enforce **server-side autoritativo** en `SY.admin::enforce_cloud_relay_content` (sólo origen `IO.cloud@hive`,
    justo tras `authorize_cloud_relay`, sin bypass) + espejo en `io.cloud::translate_cloud_op` (error temprano
    limpio). Sólo se ata el origen del relay Cloud; los internos SY.* siguen escribiendo esas keys normal.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.24` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.24` instalado; **io.cloud + sy-admin + sy-identity + sy-vault `active`**,
  io.cloud `NRestarts=0`, **0 units `failed`**. Pre-deploy: tests en las 3 capas verdes (SDK vault, io.cloud
  translate, admin enforce) + verificación de wiring (enforce corre siempre tras authorize, sin bypass).
- **Pendiente (no de este deploy):** el diagnóstico del error de `create_tenant` desde Cloud (se ve **mañana con
  el dev** — el alta funciona backend-side; el error está en el round-trip de respuesta cross-hive edge↔io.cloud).
- **Rollback:** snapshot VM100 `pre-vault-guard-0-1-24`, o `apt-get install -y --allow-downgrades fluxbee=0.1.23`.

---

## 0.1.23 — io.cloud: framework de acciones Cloud de primera clase (relay + local) + `register_human` con tenant en la raíz

- **Fecha:** 2026-08-21 (ART) · **Versión anterior:** 0.1.22 · **Commit:** `4abad1b` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `fluxbee_sdk::cloud` (el vocabulario Cloud compartido) + el nodo runtime `IO.cloud`.
- **Qué cambió (impacto operativo):**
  - `register_human` deja de ser una rama ad-hoc (`op == "register_human"`) y pasa a ser una **acción Cloud
    de primera clase**, despachada por el **set declarado** en el SDK (`CLOUD_LOCAL_OPS`), no por string mágico.
    Dos categorías: **relay** (las 3 de siempre → `SY.admin`) y **local** (`register_human`, `list_cloud_actions`
    → las resuelve io.cloud, **nunca** tocan admin), disjuntas por diseño (un test lo fija — un op local no
    puede filtrarse al gate `authorize_cloud_relay`).
  - **Nueva acción `list_cloud_actions`** (local): devuelve el catálogo de acciones (relay + local) con help,
    para que Fluxbee Cloud **descubra la superficie** sin depender del doc. Cierra el "no discovery API".
  - **Sobre de `register_human` alineado:** el `tenant_id` ahora va en la **raíz** del sobre (como
    put_token/provision_node), ya no en `params`; io.cloud lo inyecta al `frontdesk_handoff` para el frontdesk.
    Es **breaking** vs 0.1.22, pero Cloud aún no tiene nada firme construido y se adapta.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.23` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.23` instalado; **io.cloud `active/running`, `NRestarts=0`, `ExecMainStatus=0`**
  (conectó al router, ilk propio, ICH `ich:14b66389…` habilitado — sin FAILED_CONFIG); `sy-admin` +
  `sy-orchestrator` + `sy-frontdesk-gov` + los 13 `sy-*` + `rt-gateway` `active/running`; los 9 `fluxbee-node-*`
  `active/running`; **0 units `failed`**. Pre-deploy: SDK cloud tests + workspace io verdes; revisión adversarial
  (2 lentes + verify) → **0 defectos confirmados** (+ 1 hardening de drift del catálogo aplicado).
- **Pendiente:** **E2E funcional** de `register_human` desde Fluxbee Cloud dev (mañana) — no ejecutable en vivo
  desde acá (sin bearer). Contrato final para Cloud en `docs/io-cloud-api.md` §4.4 (`register_human`) / §4.5
  (`list_cloud_actions`).
- **Rollback:** snapshot VM100 `pre-cloud-actions-0-1-23`, o `apt-get install -y --allow-downgrades fluxbee=0.1.22`.

---

## 0.1.22 — frontdesk: fix del bug conversacional + extensión de datos del ilk humano

- **Fecha:** 2026-08-20 (ART) · **Versión anterior:** 0.1.21 · **Commit:** `15fc77f` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `sy-frontdesk-gov` (systemd) + `io_common` (comentario).
- **Qué cambió (impacto operativo):**
  - **BUG arreglado:** el camino conversacional del frontdesk (alcanzable — el force rule 0.1.20 manda el
    mensaje plano de un humano de primer contacto al frontdesk) reportaba `REGISTERED`/`complete`
    **sin haber registrado** cuando el turno del LLM no escribía `thread_state` (turno de charla, o
    registro-y-borrado, ambiguos). Ahora: `None` → `needs_input`/`IN_CONVERSATION` (no falso REGISTERED),
    y el prompt persiste `status=completed` en el éxito (en vez de borrar) para desambiguar. El camino
    determinista (io.cloud `register_human`) **no se toca** (devuelve el resultado del registro directo).
  - **Schema del ilk humano extendido (aditivo, CERO cambio en identity — `identification` es JSONB
    libre guardado verbatim):** `company_name` (typed) + `attributes` (libre) fluyen end-to-end a
    `ILK_REGISTER` por la tool compartida → sirve a los dos caminos. `handle_frontdesk_handoff` antes
    **tiraba** `company_name`; ahora lo reenvía + mergea `attributes` multi-turno (simétrico con company_name).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.22` → 245 MB, 118 entradas.
- **Publish:** `apt-repo-publish.sh` → repo :8900 (22 versiones).
- **Verificación en vivo:** `fluxbee 0.1.22` instalado; **`sy-frontdesk-gov` reiniciado + `running`**
  (02:31 UTC, con el fix+schema); rt-gateway + los 13 `sy-*` `active`; io.cloud `active`; **0 nodos `failed`**.
  Pre-deploy: `sy-frontdesk-gov` tests verdes (26/0, incluye el test que codificaba el bug, corregido) +
  revisión adversarial (bug-fix limpio; 1 LOW de simetría de `attributes` multi-turno, arreglado).
- **Rollback:** snapshot VM100 `pre-frontdesk-0-1-22`, o `apt-get install -y fluxbee=0.1.21`.
- **Pendiente / known:** el camino conversacional depende de que el LLM siga el prompt (misma
  confiabilidad que todo el flujo); un `attributes` libre sin cota de tamaño podría meter blobs grandes
  en el JSONB (validación de tamaño = mejora futura si hace falta). Cierra los dos follow-ups del 0.1.21.

## 0.1.21 — io.cloud `register_human`: registro automático de humanos Cloud→frontdesk

- **Fecha:** 2026-08-20 (ART) · **Versión anterior:** 0.1.20 · **Commit:** `2f60403` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). io.cloud es un runtime managed motherbee-only; los spokes no lo corren.
- **Qué cambió (impacto operativo):**
  - Nuevo op inbound `register_human` en io.cloud (NO es relay a SY.admin): Fluxbee Cloud manda la data
    del humano como `frontdesk_handoff` JSON; io.cloud **provisiona** un ilk temporary humano (mismo
    `strict_provision_ilk` que io.api) y **Unicastea** el handoff **verbatim** al frontdesk configurado
    (`government.identity_frontdesk`). El frontdesk (path determinista) registra (temporary→complete) y
    responde; io.cloud **relaya el veredicto estructurado** a Cloud, estampado con el `ilk_id` que minteó.
  - **Unicast, no el force rule del router**: un handoff explícito llega al frontdesk con CUALQUIER estado
    del ilk, así que un re-registro de un humano ya `complete` igual aterriza (`ILK_REGISTER` idempotente).
    io.cloud no elige target (usa el frontdesk de config); el force 0.1.20 queda como red de contención de
    emisores implícitos (io.slack/io.wapp).
  - Gate estricto (type + schema_version + operation + tenant `tnt:<uuid>` canónico + email real) y
    `response_envelope` (para que el frontdesk emita el veredicto estructurado, no texto plano).
  - De-diverge: `frontdesk_response_contract` compartido en io_common; io.api de-divergido.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.21` → 245 MB, 118 entradas.
- **Publish:** `apt-repo-publish.sh` → repo :8900 (21 versiones indexadas).
- **Verificación en vivo:** `fluxbee 0.1.21` instalado; runtime **io.cloud movido 0.1.20→0.1.21 + reiniciado
  + sano** (conectado al router como `IO.cloud@motherbee`, self-ilk, ICH habilitado); rt-gateway ruteando;
  13 `sy-*` + rt-gateway `active`; **0 nodos `failed`**. Pre-deploy: io workspace verde (rust 1.92) +
  **doble revisión adversarial** (10 hallazgos 1ra pasada, todos arreglados en la fuente; re-review limpia).
- **Rollback:** snapshot VM100 `pre-register-human-0-1-21`, o `apt-get install -y fluxbee=0.1.20`.
- **Pendiente / known:** (a) **test E2E funcional** de `register_human` (que Cloud lo llame de verdad) —
  el binario corre sano, falta el disparo desde Fluxbee Cloud. (b) 🔖 bug del path conversacional del
  frontdesk (reporta REGISTERED sin registrar). (c) 🔖 `company_name`/`attributes` que el determinista tira
  hoy (extensión del schema del ilk humano). (b)+(c) son el próximo paso.

## 0.1.20 — router: ruteo al frontdesk como política OPA (sin fallback Rust)

- **Fecha:** 2026-08-20 (ART) · **Versión anterior:** 0.1.19 · **Commit:** `1297ba7` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100) únicamente. Los spokes (worker1/ingress/egress) siguen en
  **0.1.19** — el `.deb` sólo actualiza motherbee y el core-update a spokes es opcional (misma wasm
  de autoridad ⇒ decisiones idénticas; el ruteo al frontdesk ocurre en motherbee, donde vive la identidad).
- **Qué cambió (impacto operativo):**
  - El ruteo "emisor sin identificar → frontdesk" dejó de ser un `if` hardcodeado en el router
    (`apply_identity_pre_resolve`) y pasó a ser una regla **visible** en `policy/system.rego`
    (nuevo entrypoint `fluxbee/system/frontdesk_route`). Ahora caen al frontdesk **tanto** los ilk
    `temporary` **como** los mensajes **sin ilk** (antes el sin-ilk se perdía en `OpaError::NotLoaded`).
  - Se **eliminó** el fallback Rust `authority()` (duplicaba la política de autoridad SYSTEM):
    **fuente única = el rego**, y el router **falla-cerrado al arrancar** si un `.wasm` horneado no
    carga (`ensure_system_policy_loaded` → `RouterError::Startup`).
  - El re-check `SO-05` del orchestrator ahora usa `authorize_system` (misma política, no un gemelo Rust).
- **Build:** fb-build (VM110), `packaging/build-deb.sh 0.1.20` → `fluxbee_0.1.20_amd64.deb`, 245 MB,
  118 entradas; los `.wasm` van **horneados dentro de `rt-gateway`** (`include_bytes!`), no como archivos.
- **Publish:** `scripts/apt-repo-publish.sh --deb …0.1.20….deb` → `/var/lib/fluxbee-apt` servido en
  `10.10.10.50:8900` (con `-m`, conserva versiones para rollback).
- **Verificación en vivo (post-`apt install`):**
  - `rt-gateway.service` `active (running)` — **arrancó fail-closed OK** (`ensure_system_policy_loaded`
    pasó: los dos wasm horneados cargaron) y **rutea tráfico real** (ADMIN_COMMAND / INVENTORY / VAULT_GET).
  - 13 `sy-*` + `rt-gateway` `running`; **0 nodos `failed`**.
  - Router `/status` **sin user-OPA catchall** ⇒ None→frontdesk es mejora estricta (no redirige tráfico existente).
  - Pre-deploy: tests lib+bins verdes (rust **1.92.0**) + **revisión adversarial** (4 lentes + verificación): **0 defectos confirmados**.
- **Rollback:** snapshot VM100 `pre-router-0-1-20`, o `apt-get install -y fluxbee=0.1.19`.
- **Pendiente / known:** el frontdesk aún **no puede MINTear** un ilk para un emisor totalmente
  sin-ilk (`ILK_PROVISION` es IO-only; `ILK_REGISTER` sólo COMPLETA un temporary existente) — el
  sin-ilk llega al frontdesk pero todavía no se onboardea. Fast-follow separado.

---

<!-- PLANTILLA para la próxima entrada (copiar arriba de esta línea, más reciente primero):

## 0.1.N — <título corto del cambio>

- **Fecha:** YYYY-MM-DD (ART) · **Versión anterior:** 0.1.N-1 · **Commit:** `<sha>` (branch `<rama>`)
- **Alcance:** motherbee | + spokes (worker1/ingress/egress)
- **Qué cambió (impacto operativo):** <1-3 bullets, en términos de qué hace distinto el sistema>
- **Build:** fb-build (VM110), `build-deb.sh 0.1.N` → tamaño / entradas / preflight OK
- **Publish:** `apt-repo-publish.sh` → repo :8900
- **Verificación en vivo:** <servicios `running`, 0 `failed`, chequeo funcional específico del cambio>
- **Rollback:** snapshot `pre-<algo>` · `apt install fluxbee=0.1.N-1`
- **Pendiente / known:** <lo que queda>

-->
