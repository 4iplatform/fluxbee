# SY.vault implementation tasks

Source spec: `docs/sy-vault-spec.md`

Status: implementation task list after design decisions closed.

Goal: introduce `SY.vault@<hive>` as the canonical encrypted-at-rest secret store for Fluxbee. Once enabled, new secret writes go through vault, not node-local `secrets.json`.

## 1. Current context

- Fluxbee already has node-local secret persistence through `crates/fluxbee_sdk/src/node_secret.rs`; this becomes legacy once vault is enabled.
- `CONFIG_GET` / `CONFIG_SET` are already the canonical way to discover and apply node-owned secret-bearing config.
- `SY.identity`, `SY.storage`, `SY.architect`, AI and IO nodes already use or are being aligned to node-local `secrets.json`.
- The vault spec introduces a global/per-hive secret service and replaces node-local secret writes as one monolithic alpha change.

## 2. Implementation direction

Implement `SY.vault` as a separate core system service:

- binary: `sy-vault`
- L2 node name: `SY.vault@<hive>`
- data file: `/var/lib/fluxbee/vault.db`
- master key file: `/etc/fluxbee/vault.master.key`
- no HTTP API
- no prompt-specific behavior
- admin/archi access goes through formal admin actions and help metadata, not through prompt instructions

The first implementation should focus on correctness and operability in a single hive. Cross-hive vault federation and transport encryption remain out of scope. Orchestrator manages `sy-vault` as a normal SY service, but other nodes must not block on vault readiness; they start degraded/unconfigured until required secrets exist.

## 3. Closed design decisions

### D1. Caller identity and authorization

Vault authorizes by canonical `meta.src_ilk`. It enriches callers from identity SHM when available, but root-tenant system reads must also work without SHM by comparing against deterministic SY ILKs computed from `hive.yaml`. Vault does not use SHM for secret data.

### D2. Tenant IDs and keys

Secret metadata uses canonical `tenant_id = tnt:<uuid>`. Infrastructure-wide secrets use the fixed Fluxbee root tenant `tnt:00000000-0000-0000-0000-000000000001`. Key text may use human-readable slugs, but authorization never parses tenant identity from the key.

### D3. Audit storage

Audit is local to vault in `/var/lib/fluxbee/vault.db`, in the same SQLite file as secrets. No NATS, no `SY.storage`, no external DB.

### D4. PUT idempotency

Same-value `VAULT_PUT` does not increment version and writes audit as `result = "noop"`.

### D5. SQLite implementation

Use SQLite through `rusqlite`.

### D6. Encryption

Use AES-256-GCM and a 32 raw-byte master key in `/etc/fluxbee/vault.master.key`.

### D7. Orchestrator behavior

`sy-vault` is installed and managed as a normal SY service. Orchestrator should start it, but it should not impose a strict startup sequence that blocks AI/IO/SY nodes. Consumers start degraded/unconfigured and retry/read when vault becomes available.

### D8. Admin-centralized writes

Secret writes are centralized in admin:

- nodes expose required vault resources through `CONFIG_GET`;
- admin receives secret-bearing config/actions;
- admin writes plaintext secret values to vault;
- nodes receive/store only non-secret config;
- nodes consume secrets through SDK vault helpers by `resource_type`, tenant, and optional dedicated ILK.

### D9. SDK retry and first consumer

The SDK must include retry/resource helpers. `SY.architect` is the first consumer used to validate the full pattern: start without secret, retry/read from vault, then become configured once the secret exists.

### D10. Archi integration

Archi should not learn vault behavior via prompt detail.

Required:

- admin actions expose formal help schemas for vault operations;
- plan compiler can generate `vault_put`, `vault_get_metadata`, `vault_list`, etc.;
- secret values must be treated as sensitive in admin action preview/history.

### D11. Audit query

Spec says there is no L2 audit query in v1. That is acceptable for backend minimalism, but limits Archi/operator visibility.

Decision:

- do not add audit query in first implementation;
- document direct SQLite operator procedure;
- add an explicit audit read verb/admin action later if UI needs it.
- v1 only writes audit locally; read/query UX is a later cross-system audit topic.

### D12. Alpha reset behavior

In alpha, `fluxbee_cleanall.sh` may delete both `/var/lib/fluxbee/vault.db` and `/etc/fluxbee/vault.master.key` without additional confirmation. This matches the current full-reset testing workflow.

### D13. Service user

`sy-vault` uses the same service user/ownership model as the existing SY services. Do not introduce a dedicated `sy-vault` user in v1.

### D14. SDK retry defaults

Default `vault_get_with_retry` policy:

- max elapsed: `60s`;
- initial delay: `250ms`;
- max delay: `5s`;
- jitter: `20%`;
- retry on `VAULT_UNAVAILABLE`, timeout, and `KEY_NOT_FOUND`;
- do not retry on `UNAUTHORIZED`, `INVALID_KEY_FORMAT`, or `INVALID_VAULT_REF`.

## 4. Phase A - Spec alignment

- [x] VA-A1. Update `docs/sy-vault-spec.md` to match current protocol fields: `routing.src_l2_name` and `meta.src_ilk`.
- [x] VA-A2. Decide and document caller tenant resolution: identity SHM read by `meta.src_ilk`.
- [x] VA-A3. Clarify metadata tenant format: `tenant_id = tnt:<uuid>`; infrastructure-wide secrets use the fixed Fluxbee root tenant.
- [x] VA-A4. Clarify key naming: key text is operator-facing and not authoritative for authorization.
- [x] VA-A5. Resolve PUT idempotency vs audit contradiction: no version increment, audit as `noop`.
- [x] VA-A6. Define max secret value size in one place. Initial hard limit: 1 MiB.
- [x] VA-A7. Define response envelopes consistently with current L2 system message style.

## 5. Phase B - Rust SDK vault contract

- [x] VA-B1. Add `crates/fluxbee_sdk/src/vault.rs`.
- [x] VA-B2a. Define initial verb constants:
  - `VAULT_PUT`
  - `VAULT_GET`
  - `VAULT_GET_METADATA`
- [x] VA-B2b. Define remaining verb constants:
  - `VAULT_LIST`
  - `VAULT_DELETE`
  - `VAULT_ROTATE`
  - `VAULT_ROLLBACK`
- [x] VA-B3a. Define request/response structs for initial verbs.
- [x] VA-B3b. Define request/response structs for list/delete/rotate/rollback.
- [x] VA-B4a. Define `VaultMetadata` and typed vault errors.
- [x] VA-B4b. Define `VaultFilter` for listing/querying.
- [x] VA-B5a. Implement `vault_get` / `vault_get_metadata` L2 helper functions using `NodeSender` and trace-id matching pattern used by identity helpers.
- [x] VA-B5b. Implement SDK helpers for `VAULT_PUT`, `VAULT_LIST`, `VAULT_DELETE`, `VAULT_ROTATE`, and `VAULT_ROLLBACK`.
- [x] VA-B6. Re-export vault types/helpers from `fluxbee_sdk::lib` and prelude if appropriate.
- [x] VA-B7a. Historical vault-ref parser tests existed during early implementation; active Model D' testing moved to resource-type serialization/normalization.
- [x] VA-B7b. Add unit tests for serialization and response parsing. **Hecho** — `ResourceType` serde + `VaultListResponse` error parsing covered in SDK tests.
- [x] VA-B8. Historical `vault_ref` parsing was removed from the active Model D' path; consumers now resolve by `resource_type` + tenant/ILK, not refs.
- [x] VA-B9. Implement `vault_get_with_retry` with bounded attempts/time, backoff, and jitter.
- [x] VA-B10. Ensure retry does not retry `UNAUTHORIZED`, invalid key/resource errors, or malformed vault responses.

## 6. Phase C - `sy-vault` node implementation

- [x] VA-C1. Add `src/bin/sy_vault.rs`.
- [x] VA-C2. Use Cargo auto-discovered binary target `sy_vault` and install/service name `sy-vault`.
- [x] VA-C3. Load hive id and node name using the same core service pattern as other SY nodes.
- [x] VA-C4. Implement lock file `/var/run/fluxbee/sy-vault.lock`.
- [x] VA-C5. Implement master key load/generate:
  - create if missing;
  - validate exact 32 bytes;
  - enforce `0600` or stricter;
  - fail closed on invalid key.
- [x] VA-C6. Implement SQLite schema creation and schema version check.
- [x] VA-C7. Implement AES-256-GCM encrypt/decrypt.
- [x] VA-C8. Implement `VAULT_PUT`.
- [x] VA-C9. Implement `VAULT_GET`.
- [x] VA-C10. Implement `VAULT_LIST`.
- [x] VA-C11. Implement `VAULT_DELETE`.
- [x] VA-C12. Implement `VAULT_ROTATE`.
- [x] VA-C13. Implement `VAULT_ROLLBACK`.
- [x] VA-C14. Implement `VAULT_GET_METADATA`.
- [x] VA-C15. Ensure secret values are never logged.
- [x] VA-C16a. Add structured audit write for supported operations, including errors and no-op.
- [ ] VA-C16b. Ensure auth failures that happen before caller resolution also produce an audit row.
- [x] VA-C17. Update `last_accessed_at` and `access_count` only after successful authorized GET.
- [ ] VA-C18. Add graceful shutdown.
- [x] VA-C19. Ensure audit writes are in the same SQLite transaction as secret mutation when applicable.

## 7. Phase D - Authorization

- [x] VA-D1. Implement caller extraction from current message shape:
  - `routing.src_l2_name`
  - `meta.src_ilk`
  - `routing.trace_id`
- [x] VA-D2. Resolve caller tenant and ILK type through identity SHM by canonical `meta.src_ilk`.
- [x] VA-D3. Implement admin authorization for deterministic `SY.admin` and `SY.architect` ILKs.
- [x] VA-D4. Implement dedicated-secret authorization: caller `meta.src_ilk == metadata.ilk`.
- [x] VA-D5. Implement same-tenant pool authorization when caller tenant is resolved from SHM.
- [x] VA-D6. For infrastructure-wide secrets in the fixed Fluxbee root tenant, enforce vault policy authorization.
- [x] VA-D7a. Prevent key enumeration for `VAULT_GET`.
- [x] VA-D7b. Prevent key enumeration for `VAULT_LIST`.
- [x] VA-D8. Unit-test allowed/denied matrix.

## 8. Phase E - Admin and Archi integration

- [x] VA-E1a. Add admin action handlers for `vault_put`, `vault_get`, and `vault_get_metadata` that proxy to `SY.vault` over L2.
- [x] VA-E1b. Add admin action handlers for `vault_list`, `vault_delete`, `vault_rotate`, and `vault_rollback`.
- [x] VA-E2a. Add action help entries for:
  - `vault_put`
  - `vault_get`
  - `vault_get_metadata`
- [x] VA-E2b. Add action help entries for:
  - `vault_list`
  - `vault_delete`
  - `vault_rotate`
  - `vault_rollback`
- [x] VA-E3. Mark value-bearing actions as secret-sensitive in previews/logs.
- [x] VA-E4. Ensure executor plan display redacts `value`.
- [x] VA-E5a. Add SCMD examples for `vault_put`, `vault_get`, and `vault_get_metadata` to admin help reference.
- [x] VA-E5b. Add SCMD examples for list/delete/rotate/rollback after those verbs exist.
- [x] VA-E6. Add Archi handbook guidance at method level only:
  - discover action help;
  - do not print secret values;
  - prefer metadata/list operations for inspection.
- [x] VA-E7. Model D' supersedes the ref-forwarding path: admin writes secret values to vault, consumers discover by `resource_type`; node `CONFIG_SET` rejects secret-bearing fields instead of receiving refs.
- [x] VA-E8. `CONFIG_GET contract.resources` / `resources.*` is the source of truth for required vault resources in migrated consumers.
- [ ] VA-E9. Keep admin action/result payloads redacted even when the operator supplied plaintext in the request.

## 9. Phase F - Install, orchestrator, and core manifest

- [x] VA-F1. Add `sy-vault` compile/install path in `scripts/install.sh`.
- [x] VA-F2. Install `/usr/bin/sy-vault`.
- [x] VA-F3. Add `sy-vault` to `/var/lib/fluxbee/dist/core/bin`.
- [x] VA-F4. Add `sy-vault` to core manifest.
- [x] VA-F5. Add systemd unit for `sy-vault`.
- [x] VA-F6. Decide whether orchestrator should start it by default: yes, normal SY service.
- [x] VA-F7. Add to orchestrator-managed service set without making AI/IO/SY node startup block on vault readiness.
- [x] VA-F8. Add clean/reset behavior to `fluxbee_cleanall.sh`:
  - alpha clean deletes vault DB/key;
  - no extra confirmation in this development cycle.
- [x] VA-F9a. Add master key permission checks in vault boot.
- [ ] VA-F9b. Add database file permission/ownership checks in install or vault boot.
- [x] VA-F10. Decide service user model: same as existing SY services; no dedicated `sy-vault` user in v1.

## 10. Phase G - Active consumer migration strategy (Model D')

Active rule: consumers do not receive `vault://...` refs through CONFIG_SET. A node declares required resources in code, calls `resolve_resource(resource_type, my_tenant, my_ilk)` at boot or on demand, and handles `VAULT_SECRET_CHANGED` broadcasts for refresh/restart.

- [x] VA-G1. Define Model D' discovery convention: `resource_type` + `tenant_id` + optional `ilk`, with infrastructure secrets in `DEFAULT_ROOT_TENANT_ID`.
- [x] VA-G2. `SY.storage`, `SY.identity`, `SY.admin`, `SY.architect`, `SY.cognition`, `ai.generic`, `ai-frontdesk-gov`, and `io-slack` use vault discovery for their migrated secret resources.
- [x] VA-G3. Node CONFIG_SET handlers for migrated consumers reject secret-bearing plaintext/ref fields and keep only non-secret config.
- [x] VA-G4. Vault change delivery uses `VAULT_SECRET_CHANGED` broadcasts instead of polling loops for migrated SY consumers.
- [ ] VA-G5. Multi-tenant IO design remains open for `IO.api` and `IO.linkedhelper`; do not implement old ref-based tasks.
- [ ] VA-G6. Remove remaining SDK/node-local secret write helpers after `IO.api` / `IO.linkedhelper` stop using them.
- [ ] VA-G7. Document runtime behavior when vault/secret is missing: node runs degraded/unconfigured and refreshes/restarts according to its own runtime policy.

## 11. Phase H - Tests and diagnostics

- [ ] VA-H1. Unit-test key validation.
- [ ] VA-H2. Unit-test encrypt/decrypt roundtrip.
- [ ] VA-H3. Unit-test wrong key cannot decrypt.
- [ ] VA-H4. Unit-test DB schema creation.
- [ ] VA-H5. Unit-test current + previous version rotation.
- [ ] VA-H6. Unit-test rollback with and without previous version.
- [ ] VA-H7. Unit-test audit rows for success, denied, error, noop.
- [ ] VA-H8. Add diag binary or script for direct L2 vault smoke test.
- [ ] VA-H9. Add E2E:
  - start vault;
  - put secret as admin;
  - get metadata;
  - get value as owner;
  - deny non-owner;
  - rotate;
  - rollback;
  - delete.
- [ ] VA-H10. Verify no plaintext secret appears in logs or admin/action output.

## 12. Phase I - Documentation

- [x] VA-I1. Update `docs/sy-vault-spec.md` after decisions in Phase A.
- [ ] VA-I2. Add operator runbook:
  - init;
  - backup;
  - restore;
  - audit inspection;
  - reset in alpha.
- [ ] VA-I3. Update `docs/onworking COA/node-secret-config-spec.md` to explain vault is the new canonical secret backend and `secrets.json` is legacy.
- [ ] VA-I4. Update `docs/onworking COA/node_secret_tasks.md` with the change in direction from "no vault v1" to "vault replaces node-local secret writes".
- [ ] VA-I5. Update admin help reference after admin actions exist.
- [ ] VA-I6. Add examples for common secrets:
  - OpenAI API key;
  - Slack bot token;
  - IO.api webhook secret;
  - Postgres URL for system services.

## 12. Historical Phase J - removed from active plan

Phase J (the old `vault_put + CONFIG_SET with vault_ref` model) is no longer part of the active task list. It was replaced by Model D': resource-oriented discovery directly from vault by `resource_type`, tenant pool, and optional `ilk`.

Historical details were intentionally removed from this file to avoid implementing dead paths. The active plan starts below in Phase J'.

## 12bis. Phase J' - Vault Model D' implementation (vigente)

**Diseño de referencia:** `docs/onworking COA/sy_vault_model_d.md`. Esa es la fuente de verdad del contrato (schema del Secret, reglas de auth, discovery del consumer, ResourceType enum, regla del pool, well-known admin ILKs, etc.). Las tareas de abajo son la implementación de eso.

In-scope: `sy.storage`, `sy.identity`, `sy.admin`, `sy.architect`, `sy.cognition`. Out-of-scope explícito: `ai.generic`, IO nodes (van a Phase K').

Resumen ejecutivo del cambio respecto a Phase J legacy:

- **CONFIG_SET ya no carga secrets ni refs.** El operador hace `vault_put` y los nodos descubren por `resource_type` + `tenant_id` + `ilk`/pool al boot y por `VAULT_SECRET_CHANGED` cuando cambia un secret.
- **`owner_ilk` deja el contrato activo.** Se reemplaza por `resource_type` mandatorio + `ilk` opcional. Si `ilk=null` el secret está en el pool del tenant.
- **`is_admin()` por L2 name desaparece.** Vault computa los ILK deterministicos de admin/architect al boot (well-known set) y autoriza por match.
- **`secrets.json` para refs se va.** Los nodos no persisten nada local de secrets.

### J'-0a. Prerrequisito — env var del orchestrator para nodos dinámicos (AI/IO/WF)

Hoy el orchestrator pre-registra cada nodo dinámico llamando a `SY.identity` con `ILK_REGISTER` y persiste `node_name → ilk_id` localmente en `<state_dir>/orchestrator/identity_node_ilk_map.json`. Pero **ese `ilk_id` no llega al nodo spawneado** — el `systemd-run` solo le pasa `FLUXBEE_NODE_NAME`. Resultado: AI/IO/WF runners arrancan sin conocer su propio ILK y mandan mensajes con `meta.src_ilk: None`, inservible para auth en modelo D'.

Fix:

- [x] VA-J'-0a-1. `register_node_identity` (en `sy_orchestrator.rs` ~9760): después del `ILK_REGISTER` exitoso, retornar también `tenant_id` resuelto. **Hecho**: `identity_tenant_id` se computa en `run_node_flow` desde `resolve_tenant_id_for_node(payload)` y se persiste junto con el ILK en `IdentityNodeIlkMap.tenants` para reconcile-on-boot.
- [x] VA-J'-0a-2. `systemd-run` ahora emite `--setenv=FLUXBEE_NODE_ILK_ID` y `--setenv=FLUXBEE_NODE_TENANT_ID` cuando se pasan. `build_managed_node_run_command` toma `Option<&str>` para ambos; SY (managed por unit files estáticos) no pasa por este path así que no recibe envs extra (mantiene la semántica `wait_for_self_system_ilk_id`). Tests `build_managed_node_run_command_injects_ilk_and_tenant_when_provided` agregados.
- [x] VA-J'-0a-3. SDK: `fluxbee_sdk::identity::read_self_ilk_from_env()` y `read_self_tenant_from_env()` agregadas. Constantes `ENV_SELF_ILK_ID` y `ENV_SELF_TENANT_ID` re-exportadas desde `lib.rs` + `prelude.rs`.
- [x] VA-J'-0a-4. AI runner (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs` `run_one_config`): lee env al boot, loguea presencia/ausencia con warn, cachea en `GenericAiNode { self_ilk_id, self_tenant_id }`. Mismo cambio en `run_unconfigured_bootstrap`. Test fixture actualizado.
- [x] VA-J'-0a-5. IO runners (`io-api`, `io-slack`, `io-linkedhelper`): lectura del env al boot + log de validación. Cachear en struct interno queda para Phase K' cuando empiecen a consumir vault.
- [ ] VA-J'-0a-6. E2E en VM: spawn de un AI.generic via orchestrator → verificar que el binario recibe el env var → primer mensaje hacia vault lleva `meta.src_ilk` correcto y vault autoriza. **Pendiente test en VM.**

Notas: `sy.orchestrator` no obtiene ILK propio — no escribe ni lee vault para sí mismo, así que queda como excepción documentada.

### J'-0b. Prerrequisito — alinear `sy.frontdesk.gov` al patrón SY system

El binario en `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs` está derivado del AI runner genérico (mismo nombre de archivo, misma estructura de `run_one_config` + `GenericAiNode`). En `hive.yaml` figura como `SY.frontdesk.gov` en `system_nodes` (identity le seedea un ILK deterministico igual que los otros SY), pero el código no llama a `wait_for_self_system_ilk_id` y por lo tanto el nodo nunca conoce su propio ILK al boot.

Fix:

- [x] VA-J'-0b-1. `wait_for_self_system_ilk_id("SY.frontdesk.gov", ...)` agregado al `main()` del runner antes de cargar configs. Si falla, log + degraded (no kill).
- [x] VA-J'-0b-2. `self_ilk_id: Option<String>` agregado a `GenericAiNode` de frontdesk-gov. Propagado a través de `run_one_config` y `run_unconfigured_bootstrap` desde el main. Test fixture actualizado.
- [x] VA-J'-0b-3. **Decisión tomada**: shim mínimo (`wait_for_self_system_ilk_id` + cache en el struct AI). No se reescribe frontdesk-gov como SY puro en este ciclo. Reescritura completa al binario SY-style queda para fase posterior si justifica.
- [ ] VA-J'-0b-4. E2E en VM: tras alineación, frontdesk-gov puede mandar mensajes a vault y autoriza por su ILK deterministico. **Pendiente test en VM.**

### J'-0c. Postergado — Go SDK vault helpers

Los Go SY actuales (`sy-timer`, `sy-opa-rules`, `sy-wf-rules`) no necesitan vault en alpha (ninguno consume secrets externos hoy). El Go SDK no tiene helpers para `VAULT_PUT/GET/LIST`. Cuando aparezca el primer Go consumer (probablemente `sy-wf-rules` si los workflows en algún momento llaman providers externos), se porta `crates/fluxbee_sdk/src/vault.rs` a `go/fluxbee-go-sdk/vault.go` con paridad de funciones y enum `ResourceType`. **Fuera de Phase J' para alpha.**

### J'-1. SDK base

- [x] VA-J'-1a. Mover `deterministic_system_ilk_id` de `src/bin/sy_identity.rs` a `crates/fluxbee_sdk/src/identity.rs`. Re-export. Actualizar todos los callers (sy_identity, sy_admin's `normalize_vault_put_payload`, y los nuevos use sites en sy_vault). **Hecho** — vive en SDK + `DEFAULT_ROOT_TENANT_ID` también.
- [x] VA-J'-1b. Agregar `enum ResourceType` + `normalize_resource_type(&str) -> Result<String>` en `crates/fluxbee_sdk/src/vault.rs`. **Hecho** — enum ampliado con recursos canónicos (`openai`, `anthropic`, Google/Microsoft/Slack/CRM/devtools/payments/mail/cloud/generic auth), Serialize/Deserialize manual como string canonical, y tests de normalización/serde.
- [x] VA-J'-1c. Agregar `VaultPutRequest` / `VaultListRequest` actualizados al schema del modelo D' (`resource_type` requerido, `ilk` opcional, `owner_ilk` eliminado). **Hecho** — `VaultFilter` y `VaultMetadata` extendidos.
- [x] VA-J'-1d. Agregar SDK helper `fluxbee_sdk::vault::resolve_resource(sender, receiver, caller, hive, resource_type, my_tenant, timeout) -> Result<Option<Value>>` que implementa el match path (owned → tenant pool → root-tenant pool → None). Es la API canónica que los nodos consumer usan. **Hecho**.
- [x] VA-J'-1e. Borrar `parse_vault_ref` y `resolve_vault_ref` (modelo viejo) o marcarlos como deprecated si quedan callers transitorios. En modelo D' los nodos no manejan refs. **Hecho** — funciones + tests borrados de `crates/fluxbee_sdk/src/vault.rs`; re-exports limpiados en `lib.rs` y `prelude.rs`. `VAULT_REF_PREFIX` se mantiene porque `vault_get_with_retry` lo acepta defensivamente como fallback (no daña).

### J'-2. sy_vault: schema + auth

- [x] VA-J'-2a. Cambiar el schema SQLite de `secrets`: drop columna `owner_ilk` mandatoria, agregar `resource_type` (TEXT, NOT NULL, normalized), agregar `ilk` (TEXT, NULL = pool). Alpha = drop+recreate (no migración), sigue la política `fluxbee_cleanall.sh`. **Hecho** — `fluxbee_cleanall.sh` ahora hace wipe explícito de `vault.db` + `vault.master.key`.
- [x] VA-J'-2b. Cambiar `Caller` y `resolve_caller`: eliminar `is_admin()` por L2 name. Computar al boot `well_known_admin_ilks: HashSet<String>` con admin/architect deterministicos. Identity NO va en el set (no necesita privilegios admin). **Hecho**.
- [x] VA-J'-2c. Reimplementar `authorize_*` según §6 del model D' doc:
  - `vault_get` plaintext: `caller.src_ilk == secret.ilk` o `secret.ilk == null && caller.tenant == secret.tenant`. Sin bypass.
  - `vault_list`/`vault_get_metadata`: abierto.
  - `vault_put`: solo `caller.src_ilk in well_known_admin_ilks`.
  - `vault_delete`/`rotate`/`rollback`: admin/architect well-known **o** caller==owner.
- [x] VA-J'-2d. Implementar la regla "caller no resuelto en SHM → solo ILK-equality match" (chicken/egg para identity al boot). **Hecho** — `resolve_caller` ya no aborta, y la dedicated-match path compara ILK directamente.
- [x] VA-J'-2e. Agregar query path `vault_list(resource_type, ilk_filter, tenant_id)` con `ORDER BY created_at DESC`. Resultado: lista filtrada para el match del consumer. **Hecho** en `handle_list`.

### J'-3. sy_admin HTTP layer

- [x] VA-J'-3a. Reescribir `normalize_vault_put_payload`: ahora normaliza `resource_type` (string libre → canonical), defaultea `tenant_id` al tenant raíz fijo de Fluxbee si falta, resuelve `owner_node` → `ilk` (no `owner_ilk`). `metadata.owner_ilk` legacy queda **rechazado** con `INVALID_REQUEST` (forzar a usar `owner_node` o nada). **Hecho**.
- [x] VA-J'-3b. Borrar `extract_admin_executor_openai_api_key_ref` y los helpers relacionados — admin ya no recibe refs por CONFIG_SET. **Hecho** — reemplazado por `reject_admin_executor_secret_fields`.
- [x] VA-J'-3c. Adaptar `apply_admin_executor_config_set` para que CONFIG_SET de admin solo gestione config no-secreta (catalog mode/actions, model defaults, etc.) — los secrets se cargan via `vault_put`. **Hecho**.

### J'-4. Consumer nodes — patrón común

Para cada nodo consumer, el patrón es:

1. Declarar `const REQUIRED_RESOURCES: &[(ResourceType, &str)]` con el campo del runtime que cada resource llena.
2. En boot, después de `wait_for_self_system_ilk_id`, llamar a un helper común que itera sobre `REQUIRED_RESOURCES`, hace `resolve_resource()`, y llena el runtime. Si alguno retorna None, el nodo arranca degraded para esa capacidad.
3. `VAULT_SECRET_CHANGED` broadcast que dispara refresh/restart según capacidad del nodo.
4. `CONFIG_GET` reporta `{resources: {<type>: {resolved, source, vault_key, version}}}`.
5. Borrar la persistencia local del ref (`secrets.json` para secrets se va) y los extractores de `*_ref` en CONFIG_SET.

### J'-5. `sy.storage`

- [x] VA-J'-5a. `REQUIRED_RESOURCES = [(Postgres, "database.postgres_url")]`. Borrar `STORAGE_LOCAL_REF_KEY_POSTGRES_URL` y `persist_local_postgres_url_ref` / `load_local_postgres_url_ref`. **Hecho** — sin REQUIRED_RESOURCES literal (overkill para un solo resource), pero el discovery es por `resolve_resource(Postgres, …)` y las helpers locales fueron borradas.
- [x] VA-J'-5b. `apply_storage_config_set`: borrar todo manejo de `postgres_url` / `postgres_url_ref` (el field deja de existir en CONFIG_SET). El handler de CONFIG_SET pasa a manejar SOLO config no-secreta (si la tiene; storage hoy solo tenía postgres_url → CONFIG_SET puede quedar como inert con un mensaje "no secret-bearing fields, use vault_put"). **Hecho** — rechaza `postgres_url`/`postgres_url_ref` con `INVALID_CONFIG`.
- [x] VA-J'-5c. Boot path: `resolve_resource(Postgres, my_ilk, my_tenant)` → si lo encuentra, inicializa el Storage backend; si no, degraded (igual que hoy con `STORAGE_NOT_READY`). **Hecho** — strict rejection de URLs con dbname embebido (cada consumer agrega su dbname con `with_dbname`).
- [x] VA-J'-5d. Broadcast refresh. El polling loop fue borrado; storage reacciona a `VAULT_SECRET_CHANGED` con `std::process::exit(0)` para que systemd reinicie y el pool reconecte.
- [x] VA-J'-5e. `CONFIG_GET` reporta `resources.postgres`. **Hecho**.
- [ ] VA-J'-5f. E2E: operador `vault_put` con `resource_type=postgres, tenant=tnt:00000000-0000-0000-0000-000000000001` (en pool, sin ilk) → restart storage → reporta `resolved=true, source=pool`. **Pendiente test en VM**.

### J'-6. `sy.identity`

- [x] VA-J'-6a. `REQUIRED_RESOURCES = [(Postgres, "database.postgres_url")]`. Borrar `IDENTITY_LOCAL_REF_KEY_POSTGRES_URL` y persist/load equivalentes. **Hecho** — constante y helpers borrados.
- [x] VA-J'-6b. `apply_identity_config_set`: borrar manejo de `postgres_url_ref`. CONFIG_SET queda solo para config no-secreta (si aplica). **Hecho** — rechaza secret-bearing fields.
- [x] VA-J'-6c. Boot path para resolver Postgres: usar `resolve_resource(Postgres, my_self_ilk_deterministic, DEFAULT_ROOT_TENANT_ID)`. El operador asigna el secret a identity vía `owner_node: "SY.identity"` (que admin resuelve a `identity_ilk_deterministic`). Match directo sin SHM resolution. **Hecho**.
- [x] VA-J'-6d. Refresh loop como storage. **Superseded por VA-J'-13.** Polling loop borrado; identity reacciona al broadcast con `exit(0)`.
- [x] VA-J'-6e. CONFIG_GET reporta `resources.postgres`. **Hecho**.
- [ ] VA-J'-6f. E2E: operador `vault_put` con `resource_type=postgres, tenant=tnt:00000000-0000-0000-0000-000000000001, owner_node="SY.identity"` → restart identity → DB ready. **Pendiente test en VM**.

### J'-7. `sy.admin` (executor OpenAI)

- [x] VA-J'-7a. `REQUIRED_RESOURCES = [(OpenAi, "ai_providers.openai.api_key")]`. **Hecho** — discovery directo por `resolve_resource(Openai, …)`.
- [x] VA-J'-7b. Borrar `OpenAiSection.api_key_ref` del schema de admin executor config. CONFIG_SET de admin solo persiste `default_model`, `max_tokens`, `temperature`, `top_p`, `catalog.{mode,actions}`. **Hecho**.
- [x] VA-J'-7c. `build_admin_executor_ai_runtime` ahora usa `resolve_resource(OpenAi, ...)` en vez de leer `api_key_ref` del config. **Hecho**.
- [x] VA-J'-7d. Refresh loop. **Superseded por VA-J'-13.** Polling loop borrado; admin executor reacciona al broadcast llamando `refresh_admin_executor_ai_runtime` in-memory.
- [x] VA-J'-7e. CONFIG_GET reporta `resources.openai`. **Hecho**.
- [ ] VA-J'-7f. E2E con `vault_put` en pool del root tenant + restart admin → executor configured. **Pendiente test en VM**.

### J'-8. `sy.architect`

- [x] VA-J'-8a. `REQUIRED_RESOURCES = [(OpenAi, "ai_providers.openai.api_key"), (Postgres, "storage.messages_db_url")]`. **Hecho** — discovery directo por `resolve_resource` para ambos.
- [x] VA-J'-8b. Borrar `api_key_ref` y `messages_db_url_ref` del schema. Borrar `extract_*_ref`, `reject_*_plaintext`, `resolve_*_from_vault` que armé en J4. **Hecho** — reemplazado por `reject_architect_secret_fields` + `resolve_architect_openai_api_key_from_vault` / `resolve_messages_db_url_from_vault` que usan `resolve_resource`.
- [x] VA-J'-8c. `build_architect_ai_runtime` y `refresh_architect_messages_db_url` usan `resolve_resource(...)`. El messages_db se distingue del OpenAI por `resource_type=postgres`. Si hay un Postgres dedicado a architect via `ilk`, lo usa; si no, el del pool del root tenant. **Hecho**.
- [x] VA-J'-8d. Refresh loop unificado para ambos resources. **Superseded por VA-J'-13.** Polling loop borrado; architect reacciona al broadcast llamando los dos refresh in-memory cuando matchean.
- [x] VA-J'-8e. CONFIG_GET reporta `resources.{openai,postgres}`. **Hecho**.
- [ ] VA-J'-8f. E2E con dos `vault_put`s (openai en pool + postgres en pool) y verificación de archi. **Pendiente test en VM**.

### J'-9. `sy.cognition`

- [x] VA-J'-9a. `REQUIRED_RESOURCES = [(OpenAi, "ai_providers.openai.api_key"), (Postgres, "storage.postgres_url")]`. **Hecho** — discovery directo por `resolve_cognition_resource(<type>, …)`.
- [x] VA-J'-9b. Borrar `COGNITION_LOCAL_REF_KEY_OPENAI`, `COGNITION_LOCAL_REF_KEY_STORAGE_POSTGRES_URL`, `extract_*_ref`, `reject_*_plaintext`, `resolve_*_to_plaintext`, `persist_local_*_ref` (todo lo de J5 legacy). **Hecho** — todas las helpers y constantes borradas, reemplazadas por `reject_cognition_secret_fields`.
- [x] VA-J'-9c. Cognition usa el MISMO secret de pool que storage para Postgres (mismo `resource_type=postgres, tenant=tnt:00000000-0000-0000-0000-000000000001, ilk=null` en vault). Sin duplicación. **Hecho** — comparte el pool match con storage; cognition aplica `with_dbname("fluxbee_storage")` en su path.
- [x] VA-J'-9d. Refresh loop. **Superseded por VA-J'-13.** Polling loop borrado; cognition flippa `ai_secret_source` cuando llega el broadcast.
- [x] VA-J'-9e. CONFIG_GET con `resources.{openai,postgres}`. **Hecho**.
- [ ] VA-J'-9f. E2E: cognition cold boot sin secrets → operador hace 2 vault_puts (compartidos con storage/architect) → cognition arranca configurado. **Pendiente test en VM**.

### J'-AI. `ai-generic` + `ai-frontdesk-gov` (Phase J' migration de Phase J VA-J6 legacy)

- [x] VA-J'-AI-1. `ai-generic` (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs`): borrar las 4 fuentes alternativas (`load_local_openai_api_key`, control plane legacy, YAML inline, env var fallback) y reemplazarlas por `resolve_resource(Openai, self_tenant_id)` puro. Usa los envs `FLUXBEE_NODE_ILK_ID` + `FLUXBEE_NODE_TENANT_ID` inyectados por el orchestrator. `OpenAiApiKeySource` queda con dos variantes: `Vault` y `Missing`. **Hecho**.
- [x] VA-J'-AI-2. `ai-frontdesk-gov` (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`): mismo cambio que ai-generic, pero como es un SY system node, usa `DEFAULT_ROOT_TENANT_ID` (no FLUXBEE_NODE_TENANT_ID). `self_ilk_id` viene de `wait_for_self_system_ilk_id` (ya estaba). **Hecho**.
- [x] VA-J'-AI-3. Borrar `resolve_openai_api_key_source_from_effective_config` y tests asociados de ambos runners. **Hecho**.
- [ ] VA-J'-AI-4. E2E en VM: arrancar ai-generic sin secret en vault → confirmar `OpenAiApiKeySource::Missing` en NODE_STATUS_GET → operador hace `vault_put` con `resource_type=openai` (pool del root tenant) → siguiente request resuelve y responde OK. **Pendiente test en VM.**

### J'-IO-slack. `io-slack` (Phase K parcial)

- [x] VA-J'-IO-slack-1. Boot lookup via `resolve_resource(Slack, self_tenant_id)`. El secret de vault es un objeto con `app_token` + `bot_token`; helper `extract_slack_tokens_from_vault_value` valida que ambos vengan no-vacíos. **Hecho**.
- [x] VA-J'-IO-slack-2. Eliminar las cadenas legacy: env vars (`SLACK_APP_TOKEN`/`SLACK_BOT_TOKEN`) + `resolve_secret(spawn_doc, ...)` para los dos tokens. **Hecho** — el bloque de carga en `Config::from_path_or_env` ahora deja ambos en `None` y delega al lookup post-boot. Las funciones `resolve_secret` / `json_get_string_opt` siguen porque otros campos no-secret todavía las usan.
- [x] VA-J'-IO-slack-3. Refresh runtime que reusa `resolve_slack_credentials_from_vault` + `slack.reload_credentials`. Cubre vault_put post-boot y rotaciones en runtime. **Hecho**.
- [ ] VA-J'-IO-slack-4. CONFIG_SET: hoy todavía acepta tokens vía adapter contract (path legacy de Phase J). Deprecar / rechazar en el siguiente ciclo cuando linkedhelper + io-api se migren juntos (multi-tenant). **Pendiente.**
- [ ] VA-J'-IO-slack-5. E2E en VM: `vault_put` con `resource_type=slack` + restart → io-slack se conecta y manda un mensaje. **Pendiente test en VM.**

### J'-IO-linkedhelper + J'-IO-api. Multi-tenant — postergado para charla de diseño

Ambos nodos son multi-tenant (`adapters[]` en linkedhelper, `api_keys[]` en io-api con `tenant_id` + `integration_id` por entry). El patrón single-resource pool de Model D' no aplica directo: cada entry tiene su propio secret y el match por `(resource_type, tenant_id)` no es único.

Antes de codear hay que decidir:

- Cómo se identifica cada adapter/integration en vault — ¿por `ilk` deterministico derivado del `adapter_id` / `integration_id`? ¿O `resource_type` con sub-key?
- Cómo cambia el contrato HTTP de `io-api`: hoy `api_keys[]` se envía en CONFIG_SET con `token_ref`; Model D' lo movería completamente a vault, pero el cliente externo todavía necesita el plaintext.
- Cómo se enumeran los adapters/integrations sin un CONFIG_SET con la lista — ¿por discovery via `vault_list(resource_type=...)`?

**Tareas posponidas hasta la charla:**

- [ ] VA-J'-IO-linkedhelper-*. Migración multi-tenant.
- [ ] VA-J'-IO-api-*. Migración multi-tenant + bearer auth contract.

### J'-10. Sy_vault tests

- [x] VA-J'-10a. Unit tests del nuevo `authorize_*` con la matriz: dedicated owner vs different owner, pool vs cross-tenant, vault_put solo admin/architect.
- [ ] VA-J'-10b. Unit test de `resolve_caller` retornando "unresolved" cuando SHM vacía + ILK no presente; verificar que get-plaintext sigue funcionando con direct ILK-equality match.
- [ ] VA-J'-10c. Integration test: dos secrets con mismo `(resource_type, tenant, ilk=null)` y `created_at` distintos → el más reciente gana.

### J'-11. SDK cleanup

- [ ] VA-J'-11a. Borrar paths del SDK que se quedaron sin callers: `build_node_secret_record`, `save_node_secret_record_with_root`, `NodeSecretWriteOptions`, `redacted_node_secret_record` si ya nadie los usa. **Bloqueado**: `io-api/auth.rs` todavía los usa para el flujo multi-tenant; sale cuando se cierre VA-J'-IO-api.
- [ ] VA-J'-11b. Renombrar `VaultCaller` si hace falta para reflejar el modelo D' (`src_ilk` + `src_l2_name` siguen siendo lo que pasa al wire). **Decisión pendiente** — el nombre actual es claro, no urgente cambiarlo.
- [x] VA-J'-11c. Borrar `NODE_SECRET_REDACTION_TOKEN` si dejan de usarse en payloads de responses HTTP. **Mantenido** — sigue siendo el token de redacción en respuestas de admin/architect/io-common para campos secretos en logs y payloads, eso NO es legacy de Phase J. Lo que sí se borró: `parse_vault_ref` y `resolve_vault_ref` legacy del SDK (ver VA-J'-1e).

### J'-12. Documentación

- [ ] VA-J'-12a. Actualizar `docs/sy-vault-spec.md` con el modelo D' (o referenciar `sy_vault_model_d.md` como current).
- [ ] VA-J'-12b. Actualizar `docs/onworking COA/node-secret-config-spec.md`: el único path de secrets es vault con el modelo D'. CONFIG_SET solo para config no-secreta. Archivo `secrets.json` se va.
- [ ] VA-J'-12c. Examples de `vault_put` con `owner_node` y sin `owner_node` (pool) en `docs/07-operaciones.md` o el doc de operación.

### J'-14. Retry con backoff en `resolve_database_url` al boot — **pendiente, defensa adicional**

**Problema observado (2026-05-14).** En reinstalls sin `cleanall`, sy-storage y sy-identity arrancan, hacen un único `resolve_resource(Postgres, ...)` al boot, vault aún no está registrado en el router (window de ~cientos de ms entre `systemctl active` y `ANNOUNCE` completado), el lookup retorna `VAULT_UNAVAILABLE`, y el consumer queda en `secret_source = Missing` para siempre (limitación VA-J'-13).

**Fix de install.sh aplicado (mismo día):** `wait_for_router_registration("SY.vault@<hive>")` después de restartear sy-vault. Cierra la mayoría de los casos.

**Defensa adicional (este task):** en el boot path de cada consumer, hacer retry con backoff a `resolve_database_url` y equivalentes — p.ej. 3 intentos con 2s entre cada uno antes de declarar Missing. Esto cubre casos que el wait de install.sh no atrapa: orchestrator-spawned dynamic nodes (AI/IO/WF) que no pasan por install.sh, reinicios manuales con `systemctl`, crashes transitorios de vault.

Tareas:

- [ ] VA-J'-14a. `sy_storage::resolve_database_url`: retry con backoff (3×2s).
- [ ] VA-J'-14b. `sy_identity::resolve_database_url`: retry con backoff (3×2s).
- [ ] VA-J'-14c. `ai-generic::resolve_openai_api_key_with_source`: retry con backoff (3×2s) en boot path.
- [ ] VA-J'-14d. `io-slack::resolve_slack_credentials_from_vault`: retry con backoff (3×2s) en boot path.

### J'-13b. Vault self-contained al boot (sin dependencia de identity SHM) — **2026-05-14**

Después de J'-13 quedó una dependencia residual entre vault e identity al boot que rompía el reorden vault-first en `hive.yaml`:

1. **`wait_for_self_system_ilk_id`** bloqueaba vault hasta que identity escribiera SHM con vault's own ILK. Solo se usaba para loggear (descartado con `let _self_ilk_id`).
2. **`list_ilks_from_hive_config` en `resolve_caller`** poblaba `caller.tenant_id` y `caller.ilk_type` desde SHM. La regla de root-tenant pool universal en `authorize_read` requería `caller.ilk_type == "system"` (vía SHM) → si identity arrancaba después de vault, los SY callers no podían leer pool secrets hasta que identity hubiera escrito SHM.

**Fix aplicado:**

- Vault ahora computa su own ILK con `deterministic_system_ilk_id(&node_name)` (cero espera).
- `compute_well_known_system_ilks` lee `hive.yaml` directamente y construye el set completo de ILKs deterministicos de TODOS los SY system nodes. Cero dependencia de SHM.
- `authorize_read` rule (2a) reemplazada: en lugar de exigir `caller.ilk_type == "system"` (SHM), ahora acepta cualquier caller cuyo ILK esté en `well_known_system_ilks` (computado de hive.yaml). El path SHM-based queda como fallback para callers no-SY.

Con esto, vault arranca self-contained:

- Lee `vault.master.key` (filesystem)
- Abre `vault.db` (filesystem)
- Computa `well_known_admin_ilks` (admin + architect) y `well_known_system_ilks` (admin + architect + vault + todos los SY de hive.yaml) localmente
- Entra al receive loop sin esperar a nadie

Y los consumers (identity incluido) pueden leer su postgres pool secret al boot incluso antes de que identity escriba SHM, porque su ILK deterministico ya está en el set well-known de vault.

**Tareas:**

- [x] VA-J'-13b-1. Reemplazar `wait_for_self_system_ilk_id` por `deterministic_system_ilk_id(&node_name)` en sy_vault.rs.
- [x] VA-J'-13b-2. Función `compute_well_known_system_ilks(config_dir, hive_id, self_ilk, admin_set)` que lee `system_nodes.<role>.nodes` de hive.yaml y mapea con `deterministic_system_ilk_id`.
- [x] VA-J'-13b-3. `authorize_read` toma `well_known_system_ilks` y lo usa para root-tenant pool universal sin requerir SHM.
- [x] VA-J'-13b-4. `handle_vault_message` propaga el set a `handle_get`. `handle_put/rotate/delete/rollback` no necesitan (solo well_known_admin_ilks).
- [x] VA-J'-13b-5. **Self-ILK determinístico propagado a los 7 SY nodes restantes** (sy_config_routes, sy_admin, sy_architect, sy_storage, sy_cognition, sy_policy, ai-frontdesk-gov). Antes solo vault e identity computaban su propio ILK localmente; los demás esperaban hasta 30s a que identity escribiera SHM. Ahora todos los SY arrancan self-contained: `let self_ilk_id = deterministic_system_ilk_id(&node_name)`. Cero functional change (mismo valor de ILK) + cero wait al boot.
- [x] VA-J'-13c-1. **Sentinel `"sys"` eliminado.** Toda referencia a `tenant_id = "sys"` reemplazada por `DEFAULT_ROOT_TENANT_ID` (`tnt:00000000-0000-0000-0000-000000000001`, alias `fluxbee`). Cambios: SDK (`resolve_resource`, `VaultSecretChangedPayload::matches_interest`, `read_self_tenant_from_env`), sy_vault (`authorize_read`, `validate_metadata`), sy_admin (default + docstrings + ejemplos + test fixture), handbook §4.5. Modelo conceptual más simple: todos los `tenant_id` siguen `tnt:<uuid>`, sin sentinels. Los secrets de infraestructura viven en el tenant raíz, los de clientes en sus tenants respectivos.
- [x] VA-J'-13c-2. **Bootstrap broadcast on vault boot.** `emit_bootstrap_secret_broadcasts` corre una vez después de que vault conecta al router y antes del receive loop, emitiendo un `VAULT_SECRET_CHANGED { op=put }` por cada secret en vault.db.
- [x] VA-J'-13c-3. **Orden de arranque INVERTIDO: vault al final.** El router (`src/router/mod.rs:1066`) entrega broadcasts **solo a nodos registrados al momento del envío** — no hay buffering para nodos futuros. Si vault arranca primero y emite bootstrap, los broadcasts se pierden porque ningún consumer está aún en el registry. Solución: vault arranca **al final** de los SY system nodes en `hive.yaml` y en `INSTALL_RESTART_SERVICES`. Todos los consumers ya están registrados cuando vault emite. La validación del orchestrator (`validate_system_nodes`) ahora exige `consumer_idx < vault_idx`. Consumers toleran `VAULT_UNAVAILABLE` durante su lookup inicial (quedan degraded), el bootstrap broadcast los rescata cuando vault arranca.
- [x] VA-J'-13b-6. Test unitario: caller con ILK en `well_known_system_ilks` y SHM vacía puede leer un secret con `tenant_id=DEFAULT_ROOT_TENANT_ID, ilk=null`.

### J'-13. Vault broadcast de cambios de secret — **implementado 2026-05-14**

> **Estado: completado.** SY.vault publica `VAULT_SECRET_CHANGED` por router broadcast después de cada `put` / `rotate` / `delete` / `rollback` que cambia estado. Los 5 consumers escuchan en su `process_router_message` / `process_system_message` / `handle_system_command` existente, filtran por interest (`resource_type`, `tenant_id`, `ilk`) usando `VaultSecretChangedPayload::matches_interest`, y reaccionan según su capacidad:
>
> - **sy.storage** y **sy.identity** (pool de Postgres): `std::process::exit(0)` para que systemd reinicie y el pool reconecte con el secret fresh. Cierra definitivamente el race "boot sin secret → secret aparece después" que requería restart manual.
> - **sy.admin executor** (OpenAI): `refresh_admin_executor_ai_runtime` in-memory; cero downtime.
> - **sy.architect** (OpenAI + Postgres-messages-db): `refresh_architect_ai_runtime` + `refresh_architect_messages_db_url`; cero downtime.
> - **sy.cognition** (OpenAI + Postgres-rebuild lazy): flip de `ai_secret_source` para openai; postgres se consulta lazy en el próximo rebuild.
>
> **Los 5 polling loops de 60s + el flag `live_in_vault` fueron borrados**. El broadcast es ahora la fuente de verdad sobre cambios.

The preliminary polling/NATS design is obsolete. The implemented path is router broadcast with local filtering in each consumer. Remaining work here is coverage and VM validation.

**Remaining tasks:**

- [x] VA-J'-13a. Schema final en `crates/fluxbee_sdk/src/protocol.rs`: `VaultSecretChangedPayload` (op, resource_type, tenant_id, ilk, version, key, hive_id, at_ms) + `VaultSecretOp` enum + `VaultSecretInterest` filter + `matches_interest()`. Constante `MSG_VAULT_SECRET_CHANGED`.
- [x] VA-J'-13b. Publish en `src/bin/sy_vault.rs`: `HandlerOutcome { response, broadcast }` retornado por los 4 handlers de mutación (`handle_put/rotate/delete/rollback`); dispatcher manda response y, si hay broadcast, lo emite con `Destination::Broadcast`, `scope=global`, sin auth (metadata only). put idempotente (changed=false) no emite — evita spam.
- [x] VA-J'-13c. SDK helper: en lugar de un task separado, los consumers usan su main loop existente (recv → match `MSG_VAULT_SECRET_CHANGED`) + `VaultSecretChangedPayload::matches_interest` para filtrar. Sin spawning extra, cero contención con el dispatcher.
- [x] VA-J'-13d. Storage + identity: auto-restart via `std::process::exit(0)` en match positivo. Decisión sobre hot-swap del pool: más simple, predecible y robusto que migrar 13 workers a `Arc<RwLock<Option<...>>>`. La interrupción es ~1s y systemd se encarga.
- [x] VA-J'-13e. Admin executor / architect / cognition: handler in-memory que llama a los `refresh_*` existentes. Polling loops borrados (`run_*_vault_refresh_loop` en cognition/storage/identity/admin/architect).
- [x] VA-J'-13f. Tests unitarios del filtrado: matriz cubierta (dedicated vs pool, root tenant vs tenant propio del caller) en `protocol.rs`.
- [ ] VA-J'-13g. Test E2E en VM: arrancar storage sin secret → `vault_put` → confirmar que storage pasa a `configured` sin restart manual (debería hacer exit+restart automático en <1s). **Pendiente test en VM.**

---

## 12ter. Phase K - IO nodes vault migration (deferred)

Active Model D' rule for IO nodes: no `vault://...` refs in CONFIG_SET. Each IO node must define how its integrations map to vault resources, then discover secrets by `resource_type`, tenant, and optional dedicated identity.

### K1. `IO.api`

- [ ] VA-K1a. Identify the multi-tenant auth model: API keys, webhook signing secrets, bearer tokens, tenant ownership, and integration identity.
- [ ] VA-K1b. Decide resource modeling: one resource type with integration identity, or distinct resource types such as `api_key`, `bearer_token`, `webhook`.
- [ ] VA-K1c. Remove plaintext/local secret writes once the Model D' mapping is defined.
- [ ] VA-K1d. E2E: vault_put -> IO.api discovers credential -> request HTTP authenticates correctly.

### K2. `IO.slack`

Migrated partially in J'-IO-slack. Remaining work is rejecting old CONFIG_SET token paths and VM E2E.

### K3. `IO.linkedhelper`

- [ ] VA-K3a. Design adapter-level resource mapping for `adapters[]` with tenant + installation identity.
- [ ] VA-K3b. Remove plaintext/local secret writes after the mapping is implemented.
- [ ] VA-K3c. E2E: vault_put -> linkedhelper adapter discovers credential -> API call succeeds.

### K4. Borrado final de `secrets.json` plaintext en IO

- [ ] VA-K4a. Verify no IO node writes plaintext secrets to `secrets.json`.
- [ ] VA-K4b. Remove SDK write helpers once no IO caller remains.

## 13. Current closeout slice

What remains worth doing before calling vault closed for alpha:

1. Unit coverage for `ResourceType` serialization, resource matching, authorization matrix, and most-recent pool selection.
2. VM E2E for `VAULT_SECRET_CHANGED`: storage/identity start without secret, operator `vault_put`, systemd restart/configured without manual restart.
3. Boot retry defense around `resolve_resource` for storage, identity, ai-generic, and io-slack.
4. File permission/ownership checks for `vault.db`.
5. Redaction verification for admin action previews/results/logs.
6. Documentation cleanup: `sy-vault-spec.md`, operation examples, and node-secret docs should point to Model D'.

## 14. Closed implementation constants

- Alpha reset deletes vault DB and master key.
- `sy-vault` uses the same service user/ownership model as existing SY services.
- Audit is write-only from the system perspective in v1; operators inspect SQLite directly.
- SDK retry defaults: `60s` max elapsed, `250ms` initial delay, `5s` max delay, `20%` jitter.
- SDK retries only unavailable/timeout/not-found states, never auth or malformed-input errors.
