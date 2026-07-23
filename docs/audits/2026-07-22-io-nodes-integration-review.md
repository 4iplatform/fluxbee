# io.api / io.blob / io.cloud — integration review (2026-07-22)

Deep integration review of the three IO nodes now **baked into the `.deb`** (io.api runtime +
io.blob/io.cloud singletons). Method: 7 integration lenses (edge, vault, authz, lifecycle, config,
identity, packaging) → each candidate finding **adversarially verified against the actual code** →
synthesis. 30 agents; **21 findings survived (confirmed/plausible), 1 refuted**.

**Verdict:** the audited security *cores* are solid (io.api tenant isolation / origin+ICH binding;
io.blob B1 "DMZ never peers `active/`" holds end-to-end; io.cloud degraded-OK-without-Cloud). The
gaps are **integration seams**, one HIGH. Ranked below; `[design]` = needs a product/arch call.

## FIX-1 — HIGH — io.cloud relay gate is inverted (confused-deputy → full admin)

`authorize_cloud_relay` (src/bin/sy_admin.rs:5037) is the ONLY upfront action-gate in
`dispatch_internal_admin_command`. Its logic:

```rust
if !IO_CLOUD_EXPOSED_ACTIONS.contains(&action) { return Ok(()); }  // <-- everything OUTSIDE the 3 is allowed
```

It restricts the **3 exposed actions** (`create_tenant`, `vault_put`, `run_node`) to origin
`IO.cloud@hive`, but does **not** restrict io.cloud **to** those 3. io.cloud is the sole
internet-facing relay (the node the gate exists to contain); a compromised io.cloud can relay any
OTHER `ADMIN_COMMAND` over the mesh — `vault_get`/`vault_list` (exfiltrate every tenant secret),
`delete_ilk`, `add_route`/`add_vpn`/`add_tap`, `set_node_config`, `kill_node`, `add_hive` — because
the generic dispatch drops `caller_l2_name` after this gate and the vault executes under admin's own
privileged ilk. **Fix:** default-deny for the `IO.cloud@hive` origin — permit ONLY the exposed 3,
reject everything else; leave all other callers' behavior unchanged. (`[design]` follow-up: a general
per-origin (role,action) allowlist for all mesh `ADMIN_COMMAND`s so no managed node is a confused
deputy.) **← fixing now.**

## MEDIUM

- **FIX-2 io.cloud** — the content restrictions that make the exposed writes safe (`run_node`
  IO.*-only; `vault_put` stripping caller `ilk`/`owner_ilk`/`owner_l2`/`tenant_id`) live ONLY in
  io.cloud's `translate_cloud_op` (bypassable), not re-enforced in admin. Re-enforce server-side in
  a helper invoked right after `authorize_cloud_relay` (preserve the SY.orchestrator vault_put carve-out).
- **FIX-3 cross-cutting** — SY.edge has **no `VAULT_SECRET_CHANGED` handler**: a degraded-vault edge
  boot leaves `secret=None`; when the vault comes up the edge never re-resolves, so `/e/<ich>` returns
  401 indefinitely while io.api still reports `published`. Add the arm (mirror sy_storage/sy_cognition,
  incl. router-stamped origin check). sy_edge.rs:238/324/1517/2081.
- **FIX-4 cross-cutting (io.api+admin)** — bearer rotation strands clients: `active_entry_token` is
  in-memory only; after io.api restart + edge row-loss, reconcile re-externalizes, admin blind-mints
  a fresh token and overwrites the vault, so old bearers get 401; the new token is unretrievable via
  CONFIG_GET. Fix: admin reuses the existing `edge_channel_secret:{ich}` before minting; io.api
  persists `active_entry_token` and loads it before boot reconcile.
- **FIX-5 orchestrator** — `persist_node_ilk_mapping` failure at spawn is warn-and-continue while
  run_node returns `ok`; reconcile-on-boot recovers identity ONLY from that on-disk map (no
  SY.identity fallback), so after a host reboot io.api launches with no tenant/ilk →
  `node_not_configured` for every request. Fail-loud at spawn + identity-backed reconcile fallback.
- **FIX-6 io.blob** — expired public artifacts are never reaped (expiry checked only at serve time);
  restarts re-resident expired rows, plaintext bytes persist on the DMZ, ledgers grow unbounded. Add
  an expiry check to `validate_public_artifact_row` + a periodic sweep driving the unpublish path.

## LOW / hardening / `[design]`

- **FIX-7** cross-cutting — SY.edge `bearer_matches` uses non-constant-time `==` at the DMZ frontier;
  use a constant-time compare (bounded by 244-bit token entropy). sy_edge.rs:2052.
- **FIX-8 io.blob `[design]`** — content-type is derived from producer-supplied `blob_ref.mime`, not
  bound to the actual bytes; make io.blob (the integrity authority) sniff/verify the type and be the
  source of truth (keep nosniff + sandbox-on-html).
- **FIX-9 io.blob `[design]`** — `publish_artifact` does not bind the blob_ref to the caller's tenant
  (flat active namespace); record + enforce owner tenant at the publish boundary.
- **FIX-10 io.blob `[design]`** — active-spool GC (mtime 30d) has no publication/refcount awareness;
  could delete a source a still-live publication needs for repair. Pin referenced blobs / size
  retention above max publication lifetime.
- **FIX-11 io.blob `[design]`** — B1 is enforced by deploy topology, not self-enforced: the vendored
  syncthing template ships `fluxbee-blob` as `sendreceive` on `active/` to every node's dist, and
  install.sh's seed is not role-gated. Role-gate the seed + prune stray blob/dist folders on non-mb roles.
- **FIX-12 io.cloud `[design]`** — three independent hardcoded vocabularies (IO_CLOUD_EXPOSED_ACTIONS
  vs translate_cloud_op op set vs list_cloud_actions) can drift; collapse to one source in io-common.
- **FIX-13 cross-cutting** — install.sh ignores `packaging/base-nodes.json` (hardcodes singleton/
  runtime/cargo/publish lists) despite the manifest's single-source claim; a one-line manifest edit
  silently diverges the from-source install from the `.deb`. Parse the manifest (like build-deb.sh).
- **FIX-14 cross-cutting** — install.sh's io-cloud/io-blob unit heredocs omit `TimeoutStopSec=15`
  (the `.deb` + core units carry it) → stop/upgrade can hang to systemd's 90s default then SIGKILL.
- **FIX-15 io.api** — inbound-envelope DX: `parse_api_message_request` hard-requires nested
  `{message:{text:...}}`; a top-level `{"text":..}` gets a generic "field message is required" (the
  live probe symptom). Make the errors self-documenting + restore the deleted contract-examples doc.
- **FIX-16 io.cloud `[design]`** — io.cloud's inbound loop applies no msg_type/family gate (channel
  under generic `user` family), unlike io.api's strict check; add a family gate (additive DiD).

## Refuted (1)
One candidate was refuted on verification (the code already handles it) — not carried forward.

---

## Resolución — MEDIUMs (2026-07-22)

FIX-1 (HIGH) cerrado en commit 375cddc. Los 5 MEDIUM implementados + testeados (sy_admin 86 /
sy_edge 16 / sy_orchestrator 133 verdes) + revisión adversarial:

| Fix | Qué se hizo | Dónde |
|-----|-------------|-------|
| **FIX-2** | `enforce_cloud_relay_content` server-side tras `authorize_cloud_relay`: para el origen `IO.cloud@hive`, `run_node`→IO.* y `vault_put` sin `metadata.ilk/owner_ilk/owner_l2` + `owner_node` IO.* (translate es bypassable) | sy_admin.rs + test |
| **FIX-4** | **DEFERIDO** (revisión adversarial: mi intento era un no-op). El edge YA tiene grace-window para la rotación viva (el caso común — verificado a pedido del user). El residual (restart + edge row-loss → re-mint) NO se puede cerrar del lado admin: `edge_channel_secret:{ich}` es dedicated owned-by-`SY.edge` y `authorize_read` tiene "No admin bypass" por diseño (sy_vault.rs:1391) → admin no puede leer ni reusar el token. Cerrarlo = **decisión de diseño** (token co-owned admin/edge, o read-back scoped) — no un downgrade silencioso. Documentado en el código | sy_admin.rs |
| **FIX-3** | arm `MSG_VAULT_SECRET_CHANGED` en el loop del edge que re-corre `resolve_secrets` (antes se dropeaba) → boot con vault degradado ya no deja `/e/<ich>` en 401 permanente. Origin-check fail-closed contra `SY.vault@<config.vault_hive>` (**corregido en review**: era `<own_hive>` del edge → inerte en la topología DMZ multi-hive real, porque el vault vive en motherbee, no en el ingress) | sy_edge.rs |
| **FIX-5** | (a) fallo de persist node→ilk escala a error + `identity_persist_failed` en la respuesta de run_node; (b) reconcile recupera identidad de SY.identity (`find_ilk_by_handler_node_from_hive_id`) + re-persiste cuando el mapa on-disk está vacío → io.api no arranca sin identidad tras reboot | sy_orchestrator.rs |
| **FIX-6** | `load_public_registry` dropea filas con `expires_at <= now` (antes re-residentaba expirados en cada restart). **Follow-up (parte 2):** reaper activo que borre los bytes de `public/` + prune del ledger vía unpublish/MSG_BLOB_RELEASE | sy_edge.rs |

**Pendiente (consensuado):** FIX-7..16 (LOW + decisiones de diseño de io.blob) — se encaran después.

---

## Resolución — LOW closeout (2026-07-23)

Cerrados + commiteados (batch 1 `8715e7d`, reaper `a047b40`):
- **FIX-4** — documentado como residual aceptado (vault no tiene no-clobber put; "No admin bypass"
  es invariante deliberado → admin no puede reusar el token del edge; el grace-window cubre la
  rotación viva). Cerrarlo = decisión de diseño (token co-owned admin/edge).
- **FIX-6 parte 2** — reaper activo en el edge (sweep periódico + persist del ledger podado).
- **FIX-7** — bearer constant-time compare en el frontier DMZ.
- **FIX-14** — TimeoutStopSec=15 en los units io-cloud/io-blob de install.sh.
- **FIX-15** — DX del envelope inbound de io.api (errores autodescriptivos + hoist attachments).
- **FIX-16** — family-gate en el inbound de io.cloud.

**Diferidos con razón (todos LOW/hardening, NINGUNO es bug activo — apurarlos arriesga regresión en
código compartido/load-bearing):**
- **FIX-8/9/10 (io.blob)** — content-type desde bytes / binding tenant↔blob / GC refcount-aware.
  Cambios de modelo de datos en el subsistema blob (parte en el crate compartido `fluxbee_sdk/blob`).
  Exploit acotado (read-only + hash-addressed + nosniff + sandbox-on-html). Merecen un pase enfocado.
- **FIX-11 (io.blob B1)** — B1 YA se sostiene (el add-only de `reconcile_syncthing_peer_xml` con
  `public_only` nunca comparte `active/`/`dist` con el ingress; validado end-to-end). El self-enforce
  (prune de defs sueltas) NO va en `reconcile_syncthing_peer_xml` (ahí prunear rompería a la motherbee,
  que necesita su `fluxbee-blob`) — va en el build del config del lado ingress. DiD; requiere cuidado.
- **FIX-12 (io.cloud)** — colapsar los 3 vocabularios a `io-common`. Refactor que toca el const de
  seguridad `IO_CLOUD_EXPOSED_ACTIONS` (recién endurecido en FIX-1/2). Los 3 vocabularios HOY coinciden;
  es prevención de drift, no bug. Mejor como refactor dedicado.
- **FIX-13 (install.sh)** — que parsee base-nodes.json. install.sh es el path dev-checkout (el `.deb`
  vía build-deb.sh YA lee el manifest); divergencia acotada. Refactor de shell, dev-only.

Recomendación: los diferidos son un pase enfocado y revisado, no un edit apurado al final de una
sesión larga. Ninguno bloquea el uso de los 3 nodos.
