# DEPLOYMENTS — registro de versiones desplegadas en PROD (auditoría)

> Ledger **append-only** de cada versión que TOCA producción (`pve` @ `192.168.8.207`). Es el
> registro de auditoría: **qué** versión, **cuándo**, **qué commit**, **qué cambió**, **cómo se
> verificó** y **cómo se revierte**. Una fila en la tabla + una entrada detallada por versión.
>
> **Regla (HANDBOOK §12): ningún `apt install` en prod — ni core-update a un spoke — sin una
> entrada acá.** Sin entrada, el deploy no está hecho.
>
> Hermanos: [`logbook/HANDBOOK.md`](logbook/HANDBOOK.md) (recetas de deploy), [`logbook/METHOD.md`](logbook/METHOD.md)
> (cómo se opera la infra), `logbook/YYYY-MM-DD.md` (bitácora narrativa), [`logbook/FINDINGS.md`](logbook/FINDINGS.md)
> (hallazgos/bugs). Este doc es SÓLO el ledger de versiones en prod. Fechas en **ART (−03)**;
> el journal del host prod va en EDT (ver METHOD §1).

## Tabla rápida (auditoría)

| Versión | Fecha (ART) | Commit | Alcance | Estado | Rollback |
|---|---|---|---|---|---|
| **0.1.23** | 2026-08-21 | `4abad1b` | motherbee | ✅ live | snap `pre-cloud-actions-0-1-23` · `apt install fluxbee=0.1.22` |
| **0.1.22** | 2026-08-20 | `15fc77f` | motherbee | ✅ live | snap `pre-frontdesk-0-1-22` · `apt install fluxbee=0.1.21` |
| **0.1.21** | 2026-08-20 | `2f60403` | motherbee | ✅ live | snap `pre-register-human-0-1-21` · `apt install fluxbee=0.1.20` |
| **0.1.20** | 2026-08-20 | `1297ba7` | motherbee | ✅ live | snap `pre-router-0-1-20` · `apt install fluxbee=0.1.19` |
| ≤ 0.1.19 | (pre-ledger) | — | motherbee | histórico, sin registrar | repo apt conserva 0.1.0 … 0.1.19 |

> Este ledger arranca en **0.1.20** (primera vez que se registra formalmente). Las versiones
> anteriores (0.1.0–0.1.19) se desplegaron sin ledger; el repo apt en fb-build las conserva
> (`dpkg-scanpackages -m`) para rollback, pero su detalle vive en la bitácora, no acá.

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
