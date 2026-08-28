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
