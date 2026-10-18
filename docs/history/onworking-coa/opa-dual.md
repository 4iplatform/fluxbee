# OPA-dual — two-layer policy in the router

_Estado: Fases 1–3 implementadas (2026-06-26). Fase 4 = futuro, gated on operator need._

El router es el centro de routing **y** de autoridad (decisión del operador: no
distribuir esos chequeos entre nodos). OPA-dual formaliza esto como **dos capas**
de política que el router compone:

- **Capa SYSTEM (autoritativa, no editable por usuario).** Las reglas `SY.` de
  origen-autoridad. Hoy es una tabla fija en Rust: `src/router/system_policy.rs`
  (`PROTECTED_SYSTEM_ACTIONS` + `authority(action, src_l2_name, hive_id)`). Es
  estructuralmente inalcanzable desde los endpoints `/opa/policy*` (esos solo
  llegan al blob OPA del usuario vía el nodo Go `SY.opa.rules`), así que es no
  editable por construcción — sin región SHM, archivo, ni RPC que tocar.
- **Capa USER (operador).** OPA: Rego que el operador autora → compila a WASM el
  nodo `SY.opa.rules` → el router carga en `OpaResolver` (`src/opa.rs`). Hoy
  **selecciona target de routing**; en el futuro podrá **estrechar** autoridad.

## Decisión de diseño: capa SYSTEM en Rust, no en Rego (por ahora)

Se evaluó hacer la capa system como un segundo módulo Rego/WASM. Se descartó para
esta iteración porque:

- La autoridad ya existe en Rust, testeada y validada en lab (era ~20 líneas).
- OPA hoy **no expresa allow/deny** — `resolve_target` devuelve `Option<String>`
  (un target), no un booleano. Una capa system en Rego sería un contrato nuevo
  (entrypoint boolean, parser, input nuevo, transporte nuevo) para una tabla fija
  que el operador acepta hardcodeada.
- La no-editabilidad es **gratis en Rust** (compilada) y **cara en Rego** (segunda
  región SHM + writer privilegiado + provenance/firma).

**`authority()` es un SEAM estable:** una futura capa system respaldada por Rego
puede reemplazar el backing **sin cambiar** los call-sites del router ni el orden
de composición. "OPA-dual" hoy = autoridad SYSTEM en Rust (no-override) + routing
USER en OPA, compuestos por el router.

## Semántica de composición

Dos tipos de decisión, mantenidos distintos (el código ya los separa):

- **AUTORIDAD (gate allow/deny)** — en `serialize_for_local_delivery`, para TODA
  entrega local. `final_allow = system_allow AND user_allow`. **SYSTEM DENY es
  FINAL** (el user nunca re-permite). Hoy `user_allow = true` siempre (la capa
  user de autoridad aún no existe), así que el comportamiento es idéntico al
  gate previo. `SY.orchestrator@<cualquier-hive>` (control plane cross-hive) es
  system-final y exento de cualquier user-deny futuro. Suppression = `Ok(None)`
  (drop, nunca `Err` — no tira la conexión).
- **ROUTING (selección de target)** — solo en `Destination::Resolve`. La ruta
  SYSTEM tiene precedencia y **corta** (hoy: identity pre-resolve fuerza target).
  Solo si el system no da target, corre el OPA del usuario. Ver el doc de
  `resolve_target_with_identity`.

Orden: `system-gate → system-route → user-route(OPA) → [futuro] user-authority-deny → deliver`.

## Fases

- **Fase 1 (hecha)** — nombrar la capa system: `mod system_policy` con `authority()`
  + `is_protected_system_action()`. Refactor puro, comportamiento idéntico.
- **Fase 2 (hecha)** — enriquecer el input de OPA (`build_opa_input`): agrega
  `routing.src_l2_name` (autoritativo) + `action`. Aditivo; las policies
  target-only no se afectan. Golden test fija el shape.
- **Fase 3 (hecha)** — orden de composición explícito en `resolve_target_with_identity`
  (doc, sin cambio de comportamiento).
- **Fase 4 (futuro, gated)** — capa system respaldada por Rego: segunda región
  `/jsr-opa-sys-<hive>`, writer privilegiado, entrypoint boolean de autoridad,
  guard de ownership-label contra overwrite del operador. Solo si se quiere que
  las reglas system sean inspeccionables/auditables vía OPA. El `authority()`
  signature ya es el contrato; solo cambia el backing.

## Decisiones de producto pendientes (para Fase 3+/4)

- ¿La capa user puede **estrechar** (deny) acciones protegidas (modelo
  intersección), o el user es solo-routing y la autoridad es 100% system?
- ¿La capa system llega a ser tuneable en runtime, o "hardcoded SY. está bien"
  indefinidamente? (Define si la Fase 4 se construye alguna vez.)

Ver [[router-authority-opa-dual]] (memoria) y `docs/04-routing.md`.
