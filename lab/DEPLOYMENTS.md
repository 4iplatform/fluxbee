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
| **0.1.20** | 2026-08-20 | `1297ba7` | motherbee | ✅ live | snap `pre-router-0-1-20` · `apt install fluxbee=0.1.19` |
| ≤ 0.1.19 | (pre-ledger) | — | motherbee | histórico, sin registrar | repo apt conserva 0.1.0 … 0.1.19 |

> Este ledger arranca en **0.1.20** (primera vez que se registra formalmente). Las versiones
> anteriores (0.1.0–0.1.19) se desplegaron sin ledger; el repo apt en fb-build las conserva
> (`dpkg-scanpackages -m`) para rollback, pero su detalle vive en la bitácora, no acá.

---

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
