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

Vault reads identity SHM by canonical `meta.src_ilk` to resolve caller `tenant_id`, `ilk_type`, and status. Vault does not use SHM for secret data.

### D2. Tenant IDs and keys

Secret metadata uses `tenant_id = tnt:<uuid> | sys`. Key text may use human-readable slugs, but authorization never parses tenant identity from the key.

### D3. Audit storage

Audit is local to vault in `/var/lib/fluxbee/vault.db`, in the same SQLite file as secrets. No NATS, no `SY.storage`, no external DB.

### D4. PUT idempotency

Same-value `VAULT_PUT` does not increment version and writes audit as `result = "noop"`.

### D5. SQLite implementation

Use SQLite as specified. Rust dependency still needs final crate choice, with `rusqlite` recommended for the first implementation.

### D6. Encryption

Use AES-256-GCM and a 32 raw-byte master key in `/etc/fluxbee/vault.master.key`.

### D7. Orchestrator behavior

`sy-vault` is installed and managed as a normal SY service. Orchestrator should start it, but it should not impose a strict startup sequence that blocks AI/IO/SY nodes. Consumers start degraded/unconfigured and retry/read when vault becomes available.

### D8. Admin-centralized writes

Secret writes are centralized in admin:

- nodes expose required secret fields through `CONFIG_GET`;
- admin receives secret-bearing config/actions;
- admin writes plaintext secret values to vault;
- nodes receive/store only vault references and non-secret config;
- nodes consume secrets through SDK vault helpers.

### D9. SDK retry and first consumer

The SDK must include retry/reference helpers. `SY.architect` is the first consumer to validate the full pattern: start without secret, retry/read from vault, then become configured once the secret exists.

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
- [x] VA-A3. Clarify metadata tenant format: `tenant_id = tnt:<uuid> | sys`.
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
- [x] VA-B7a. Add unit tests for vault ref parsing.
- [ ] VA-B7b. Add unit tests for serialization and response parsing.
- [x] VA-B8. Implement `vault_ref` parsing helper for `vault://<key>`.
- [x] VA-B9. Implement `vault_get_with_retry` with bounded attempts/time, backoff, and jitter.
- [x] VA-B10. Ensure retry does not retry `UNAUTHORIZED`, `INVALID_KEY_FORMAT`, or malformed refs.

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
- [x] VA-D3. Implement admin override for `SY.admin@*` and `SY.architect@*`.
- [x] VA-D4. Implement owner authorization: caller `meta.src_ilk == metadata.owner_ilk`.
- [x] VA-D5. Implement same-tenant system authorization if caller is `system` ILK.
- [x] VA-D6. For `tenant_id = sys`, allow only admin/architect.
- [x] VA-D7a. Prevent key enumeration for `VAULT_GET`.
- [x] VA-D7b. Prevent key enumeration for `VAULT_LIST`.
- [ ] VA-D8. Unit-test allowed/denied matrix.

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
- [ ] VA-E7. Change node `CONFIG_SET` secret handling path: admin writes secret-bearing values to vault and forwards/stores only `vault://...` refs.
- [ ] VA-E8. Use node `CONFIG_GET contract.secrets[*]` as the source of truth for secret-bearing fields.
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

## 10. Phase G - Consumer migration strategy

- [x] VA-G1. Define `vault_ref` convention for node config:
  - example: `config.secrets.openai.api_key_ref = "vault://sys:openai-api-key"`.
  - provider-local config may also use adjacent refs, for example `config.ai_providers.openai.api_key_ref`, when that matches the existing node contract.
- [ ] VA-G2. Deprecate new node-local plaintext `secrets.json` writes once vault is enabled. **→ Ejecutado en Phase J (VA-J1..J6 + VA-J8).**
- [x] VA-G3. Implement `SY.architect` as the first vault-backed consumer:
  - starts without OpenAI key;
  - reports missing secret;
  - retries/loads via SDK helper when key appears;
  - transitions to configured without special orchestrator ordering.
  - exposes the same vault-backed OpenAI config contract through node L2 `CONFIG_GET` / `CONFIG_SET`, so admin `node_control_config_get/set` is the canonical control path.
  - orchestrator starts configured `system_nodes` from `hive.yaml` with `SY.identity` first; `sy.identity` seeds deterministic `system` ILKs from the same list, so vault consumers must have real `ilk:<uuid>` ownership instead of placeholder/workaround metadata.
  - the `hive.yaml` schema uses the compact `system_nodes.<role>.{nodes, wait_for}` shape (service/exec/start/critical are derived/defaulted in code, not declared per entry).
  - every SY except `sy_orchestrator` resolves its own `ilk:<uuid>` at boot from identity SHM and caches it for the process lifetime, so outgoing vault calls can set `meta.src_ilk` directly without an extra identity round-trip. Rust SYs use `fluxbee_sdk::identity::wait_for_self_system_ilk_id(...)`; Go SYs (`sy-timer`, `sy-wf-rules`, `sy-opa-rules`) use the symmetric `fluxbee-go-sdk.WaitForSelfSystemIlkID(...)`.
- [ ] VA-G4. Migrate `ai.generic` OpenAI key to vault refs. **→ VA-J6 en Phase J.**
- [ ] VA-G5. Migrate IO secret-bearing configs to vault refs:
  - `IO.api` API keys/webhook secrets;
  - `IO.slack` app/bot tokens;
  - `IO.linkedhelper` adapter credentials where applicable.
  - **→ Diferido a Phase K (VA-K1..K4).** No se toca en el ciclo actual.
- [ ] VA-G6. Migrate SY DB secrets if still required:
  - `SY.storage` Postgres URL; **→ VA-J1 en Phase J.**
  - `SY.identity` Postgres URL; **→ VA-J2 en Phase J.**
  - Archi messages DB URL. **→ VA-J4 en Phase J.**
- [ ] VA-G7. Remove or mark legacy `secrets.json` write paths after consumers are migrated. **→ VA-J8 en Phase J (borrar, no marcar).**
- [ ] VA-G7a. Remove or deprecate `SY.architect` local-only config entrypoints:
  - current legacy/bootstrap paths: `GET /architect/control/config-get` and `POST /architect/control/config-set`;
  - canonical path is admin `node_control_config_get/set` -> L2 `CONFIG_GET/CONFIG_SET`;
  - local paths currently share the same handler functions, so they are not divergent, but they are an extra operational doorway and should not remain long term unless a concrete bootstrap-only need is proven;
  - if retained temporarily, rename internals away from `local_*` to neutral config handlers and label local HTTP/SCMD as deprecated in UI/docs.
- [ ] VA-G8. Document runtime behavior when vault/secret is missing: node runs degraded/unconfigured and retries/refreshes according to its own runtime policy.

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

## 12. Phase J (DEPRECATED — see Phase J' below) - Vault-only secret consumption (legacy plaintext deletion)

> **Estado: deprecado tras la conversación de diseño del 2026-05-12.** El modelo de "vault_put + CONFIG_SET con ref" se reemplaza por el modelo D' (resource-oriented secrets, discovery del consumer). Ver `docs/onworking COA/sy_vault_model_d.md` para el diseño nuevo y la sección **12bis. Phase J' (Model D')** abajo para las tareas vigentes.
>
> El código implementado para J1-J5 (storage, identity, admin, architect, cognition aceptando CONFIG_SET con `*_ref` y persistiendo en `secrets.json`) **hay que revertir/rehacer**. Lo que se conserva: el SDK helper `VaultCaller`, `wait_for_self_system_ilk_id`, y el bug-fix del global lock en admin (que es ortogonal). Los entries de abajo quedan como histórico de lo que se hizo y por qué se descarta.

## 12-legacy. Phase J - Vault-only secret consumption (legacy plaintext deletion)

Objetivo: que **todos** los servicios listados consuman secretos desde vault (vault_ref + SDK retry) y que el path legacy de CONFIG_SET con plaintext + persistencia en `secrets.json` quede **eliminado** (no deprecado-luego-eliminado: borrado directo, en línea con la política de no-legacy de este proyecto).

In-scope (este ciclo): `sy.storage`, `sy.identity`, `sy.admin`, `sy.architect (parte messages_db_url)`, `sy.cognition`, `ai.generic`.

Out-of-scope (deferido): `IO.api`, `IO.slack`, `IO.linkedhelper`. Se migran cuando se incorporen sus tokens (volver a `VA-G5` cuando toque).

Patrón canónico para todos los nodos in-scope:

1. Node `CONFIG_SET` acepta **únicamente** `vault_ref: "vault://<key>"` para campos secretos (no plaintext en el payload, no escritura a `secrets.json` del nodo).
2. Operador setea el plaintext vía `vault_put` (admin → SY.vault) usando `SY.admin` action `vault_put` antes (o después: el nodo arranca degradado y retry-loadea).
3. Node arranca con vault_ref en su config local no-secreta (es decir, el ref como string, no el plaintext).
4. Al boot/refresh, el nodo usa `fluxbee_sdk::vault::vault_get_with_retry(parse_vault_ref(ref))` para resolver el plaintext en memoria.
5. Si el secret falta o vault está caído, el nodo corre **degradado** (logueando estado claro) y reintenta según política del SDK.

### J1. `sy.storage`

- [x] VA-J1a. `CONFIG_SET` rechaza `postgres_url` plaintext con `invalid_config`, acepta solo `postgres_url_ref: vault://<key>` validado con `parse_vault_ref`.
- [x] VA-J1b. Persistencia del ref en `secrets.json` local con key `postgres_url_ref` (el archivo se mantiene por compatibilidad, pero solo contiene refs — no plaintext).
- [x] VA-J1c. `resolve_database_url` es ahora async: conecta SDK efímero a router, llama `resolve_vault_ref` contra `SY.vault@<hive>` con backoff/jitter (max 30s). Falla → arranca degradado (`STORAGE_NOT_READY`).
- [x] VA-J1d. Renombré `STORAGE_LOCAL_SECRET_KEY_POSTGRES_URL` → `STORAGE_LOCAL_REF_KEY_POSTGRES_URL`. `persist_local_postgres_url` / `load_local_postgres_url` renombradas a versiones `_ref`. Eliminado el variant `EnvCompat` + fallbacks `FLUXBEE_DATABASE_URL` / `JSR_DATABASE_URL`.
- [ ] VA-J1e. E2E: `vault_put sys:storage-postgres-url` → `CONFIG_SET storage ... postgres_url_ref` → `restart sy-storage` → `STATUS` reporta `configured`. **Pendiente test en VM.**

### J2. `sy.identity`

- [x] VA-J2a. `CONFIG_SET` rechaza `postgres_url` plaintext, acepta solo `postgres_url_ref` validado.
- [x] VA-J2b. Persistencia del ref en `secrets.json` con key `postgres_url_ref`. Sin plaintext.
- [x] VA-J2c. `resolve_database_url` async con SDK ephemeral + `resolve_vault_ref`. Si vault no responde, identity arranca in-memory + SHM ("started without active DB backend"), comportamiento actual preservado. `SY.identity` agregado al `is_admin` fast-path en `sy_vault` para resolver el chicken/egg de boot (identity es quien escribe la SHM que vault usaría para autenticarla).
- [x] VA-J2d. Renombré `IDENTITY_LOCAL_SECRET_KEY_POSTGRES_URL` → `IDENTITY_LOCAL_REF_KEY_POSTGRES_URL`, `persist_local_identity_postgres_url` / `load_local_identity_postgres_url` → `_ref`. Eliminados los fallbacks `FLUXBEE_DATABASE_URL` / `JSR_DATABASE_URL` y el variant `EnvCompat`. `_self_ilk_id` ahora se calcula al top de main con `deterministic_system_ilk_id` y se reusa en la llamada a vault.
- [ ] VA-J2e. E2E: análogo a VA-J1e con `sys:identity-postgres-url`. **Pendiente test en VM.**

### J3. `sy.admin` (executor OpenAI key)

- [x] VA-J3a. `CONFIG_SET` de admin acepta solo `config.ai_providers.openai.api_key_ref: "vault://<key>"`; rechaza plaintext con error explícito.
- [x] VA-J3b. Constante `ADMIN_EXECUTOR_LOCAL_SECRET_KEY_OPENAI` y `load_admin_secret_record` eliminadas. `apply_admin_executor_config_set` ya no escribe `secrets.json` para el executor (solo persiste el ref en el config.json local).
- [x] VA-J3c. `build_admin_executor_ai_runtime` ahora es `async`, conecta un sub-client al router, llama `resolve_vault_ref` con `VaultCaller { src_ilk: self_ilk_id, src_l2_name: SY.admin@<hive> }` contra `SY.vault@<hive>`. Si el ref no existe o vault falla → runtime degradado (`Ok(None)`). `refresh_admin_executor_ai_runtime` propaga.
- [ ] VA-J3d. E2E: `vault_put sys:admin-executor-openai-key` → `CONFIG_SET admin ... api_key_ref` → admin executor pasa a `configured` sin reiniciar. **Pendiente test en VM.**

### J4. `sy.architect` (messages_db_url, completar; OpenAI key ya migrado)

- [x] VA-J4a. `CONFIG_SET` rechaza `messages_db_url` plaintext con `reject_architect_messages_db_url_plaintext`; acepta solo `messages_db_url_ref` validado.
- [x] VA-J4b. `resolve_messages_db_url` legacy → reemplazado por `resolve_messages_db_url_from_vault` (async, ephemeral SDK + `resolve_vault_ref`). `refresh_architect_messages_db_url` ahora usa el path vault.
- [x] VA-J4c. `messages_db_connect` se ejecuta sobre el plaintext resuelto por vault. Caída silenciosa a degraded (viewer disabled) si vault no responde.
- [x] VA-J4d. Borrada toda la rama plaintext de OpenAI: campo `api_key: Option<String>` eliminado del `OpenAiSection`, `extract_architect_openai_api_key` reemplazado por `reject_architect_openai_plaintext`. Único path = `api_key_ref`.
- [x] VA-J4e. Eliminada `write_architect_openai_secret_to_vault` (auto-vault), `resolve_architect_owner_ilk` y la constante `ARCHITECT_DEFAULT_OPENAI_VAULT_REF`. `build_architect_ai_runtime` simplificado a single-path (sólo `api_key_ref` → `resolve_architect_openai_api_key_from_vault`).
- [ ] VA-J4f. E2E: `vault_put sys:architect-messages-db-url` → `CONFIG_SET architect ... messages_db_url_ref` → architect reporta `messages_db_configured: true` sin reiniciar. **Pendiente test en VM.**

### J5. `sy.cognition`

- [x] VA-J5a. `CONFIG_SET` rechaza plaintext (`reject_cognition_openai_plaintext`); acepta solo `api_key_ref` y `storage_postgres_url_ref` ambos validados con `parse_vault_ref`.
- [x] VA-J5b. Renombré `COGNITION_LOCAL_SECRET_KEY_OPENAI` → `COGNITION_LOCAL_REF_KEY_OPENAI`. Persistencia escribe el ref, no el plaintext. Eliminado `STORAGE_LOCAL_SECRET_KEY_POSTGRES_URL`, `STORAGE_NODE_BASE_NAME`, `extract_cognition_openai_api_key`. `EnvCompat` variant + `OPENAI_API_KEY` env fallback eliminados.
- [x] VA-J5c. `resolve_cognition_openai_api_key` (nueva) y `resolve_storage_database_url` ahora usan `resolve_vault_value_to_plaintext`: SDK ephemeral + `resolve_vault_ref` + extraer field anidado.
- [x] VA-J5d. Cognition ya no hace cross-read del `secrets.json` de storage. Tiene su propia config `config.storage.postgres_url_ref` que el operador apunta al mismo (o distinto) `vault://<key>` que storage usa. Loose coupling vía vault, no via filesystem.
- [ ] VA-J5e. E2E: cognition cold boot sin secret → `vault_put sys:cognition-openai-api-key` + `vault_put sys:storage-postgres-url` → `CONFIG_SET cognition ... api_key_ref + storage_postgres_url_ref` → restart → cognition converge a `configured`. **Pendiente test en VM.**

### J6. `ai.generic` — **superseded** por Phase J' Model D' (ver §12bis-AI abajo)

VA-J6a..d quedaron sin sentido en Model D': los nodos AI no manejan refs ni hacen CONFIG_SET de secrets. La migración real está cerrada en J'-AI más abajo, donde `ai-generic` y `ai-frontdesk-gov` consumen vault directo con `resolve_resource(Openai)` y borran todas las fuentes alternativas (env var, YAML inline, local file, control plane legacy).

### J7. SDK helpers — preparatorio, va PRIMERO antes de migrar nodos

Hallazgo del review técnico: `vault_get` / `vault_get_with_retry` actuales **no setean `meta.src_ilk` ni `routing.src_l2_name`**. Hoy funciona porque los únicos consumers (`SY.admin`, `SY.architect`) tienen admin-override en vault (VA-D3). En cuanto migremos `cognition`/`ai.generic`/`storage`/`identity` (system ILK + tenant `sys`), vault va a usar same-tenant system auth (VA-D5), que requiere resolver el caller por `meta.src_ilk`. Sin eso, vault niega.

- [x] VA-J7a. `crates/fluxbee_sdk/src/vault.rs` ya expone `vault_get_with_retry`, `parse_vault_ref`, request/response structs y verb constants (Phase B).
- [x] VA-J7b. Extendido. Nuevo struct `VaultCaller<'a> { src_ilk, src_l2_name }`. Todos los helpers (`vault_get`, `vault_get_metadata`, `vault_put`, `vault_list`, `vault_delete`, `vault_rotate`, `vault_rollback`, `vault_get_with_retry`) lo toman como param. `send_action_once` lo propaga al `Message` saliente (`meta.src_ilk` + `routing.src_l2_name` + `meta.target`). Único caller existente actualizado: `sy_architect::resolve_architect_openai_api_key_from_vault`.
- [x] VA-J7c. `fluxbee_sdk::resolve_vault_ref(sender, receiver, caller, hive_id, vault_ref, policy) -> Result<Value, VaultError>` agregado: parse ref + target `SY.vault@<hive>` + `vault_get_with_retry` + extrae `value`. `VaultError::EmptyValue { key }` para el caso `value == None`. Re-exportado desde `lib.rs` + `prelude.rs`.
- [ ] VA-J7d. Tests unitarios del wrapper completo (mock sender/receiver) — diferido hasta tener fake harness; los tests existentes de `parse_vault_ref` cubren el path de validación temprana.

### J8. Borrar el path legacy de CONFIG_SET plaintext en SDK

- [ ] VA-J8a. Una vez J1-J6 cerrados, eliminar `node_secret.rs` write paths de `crates/fluxbee_sdk/src/node_secret.rs` que ya nadie use (probablemente `persist_local_*` paths). Mantener solo lectura de config no-secreto local.
- [ ] VA-J8b. Borrar `NodeSecretWriteOptions` / `build_secret_write_options_from_message` si no quedan callers.
- [ ] VA-J8c. Borrar `NODE_SECRET_REDACTION_TOKEN` si ya no aparece en payloads (los nodos no exponen plaintext, solo refs).

### J9. Documentación + cleanup

- [ ] VA-J9a. Actualizar `docs/onworking COA/node-secret-config-spec.md`: el único path para secretos es vault_ref. `secrets.json` solo guarda config no-secreto (si queda algo); idealmente desaparece.
- [ ] VA-J9b. Actualizar la sección §13 (proposed first coding slice) si ya está obsoleta tras esta fase.
- [ ] VA-J9c. Cerrar/cross-referenciar `VA-G2`, `VA-G4`, `VA-G6`, `VA-G7`, `VA-G7a` contra los tasks J1-J8 correspondientes para no duplicar.

### J10. Out-of-scope explícito en este ciclo

- IO nodes (`IO.api`, `IO.slack`, `IO.linkedhelper`): se difieren a Phase K (abajo). No se tocan en este ciclo.

---

## 12bis. Phase J' - Vault Model D' implementation (vigente)

**Diseño de referencia:** `docs/onworking COA/sy_vault_model_d.md`. Esa es la fuente de verdad del contrato (schema del Secret, reglas de auth, discovery del consumer, ResourceType enum, regla del pool, well-known admin ILKs, etc.). Las tareas de abajo son la implementación de eso.

In-scope: `sy.storage`, `sy.identity`, `sy.admin`, `sy.architect`, `sy.cognition`. Out-of-scope explícito: `ai.generic`, IO nodes (van a Phase K').

Resumen ejecutivo del cambio respecto a Phase J legacy:

- **CONFIG_SET ya no carga secrets ni refs.** El operador hace `vault_put` y los nodos descubren via `vault_list` por `resource_type` + `ilk`/pool al boot + refresh periódico.
- **`owner_ilk` deja de ser mandatorio.** Se reemplaza por `resource_type` mandatorio + `ilk` opcional. Si `ilk=null` el secret está en el pool del tenant.
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
- [x] VA-J'-1b. Agregar `enum ResourceType` + `normalize_resource_type(&str) -> Result<String>` en `crates/fluxbee_sdk/src/vault.rs`. Tests unitarios de normalización. **Hecho** — enum con Serialize/Deserialize manual como string canonical.
- [x] VA-J'-1c. Agregar `VaultPutRequest` / `VaultListRequest` actualizados al schema del modelo D' (`resource_type` requerido, `ilk` opcional, `owner_ilk` eliminado). **Hecho** — `VaultFilter` y `VaultMetadata` extendidos.
- [x] VA-J'-1d. Agregar SDK helper `fluxbee_sdk::vault::resolve_resource(sender, receiver, caller, hive, resource_type, my_tenant, timeout) -> Result<Option<Value>>` que implementa el match path (owned → tenant pool → sys pool → None). Es la API canónica que los nodos consumer usan. **Hecho**.
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

- [x] VA-J'-3a. Reescribir `normalize_vault_put_payload`: ahora normaliza `resource_type` (string libre → canonical), defaultea `tenant_id` a `sys` si falta, resuelve `owner_node` → `ilk` (no `owner_ilk`). `metadata.owner_ilk` legacy queda **rechazado** con `INVALID_REQUEST` (forzar a usar `owner_node` o nada). **Hecho**.
- [x] VA-J'-3b. Borrar `extract_admin_executor_openai_api_key_ref` y los helpers relacionados — admin ya no recibe refs por CONFIG_SET. **Hecho** — reemplazado por `reject_admin_executor_secret_fields`.
- [x] VA-J'-3c. Adaptar `apply_admin_executor_config_set` para que CONFIG_SET de admin solo gestione config no-secreta (catalog mode/actions, model defaults, etc.) — los secrets se cargan via `vault_put`. **Hecho**.

### J'-4. Consumer nodes — patrón común

Para cada nodo consumer, el patrón es:

1. Declarar `const REQUIRED_RESOURCES: &[(ResourceType, &str)]` con el campo del runtime que cada resource llena.
2. En boot, después de `wait_for_self_system_ilk_id`, llamar a un helper común que itera sobre `REQUIRED_RESOURCES`, hace `resolve_resource()`, y llena el runtime. Si alguno retorna None, el nodo arranca degraded para esa capacidad.
3. Periodic refresh (default 60s) que rehace lo anterior y aplica cambios atómicamente.
4. `CONFIG_GET` reporta `{resources: {<type>: {resolved, source, vault_key, version}}}`.
5. Borrar la persistencia local del ref (`secrets.json` para secrets se va) y los extractores de `*_ref` en CONFIG_SET.

### J'-5. `sy.storage`

- [x] VA-J'-5a. `REQUIRED_RESOURCES = [(Postgres, "database.postgres_url")]`. Borrar `STORAGE_LOCAL_REF_KEY_POSTGRES_URL` y `persist_local_postgres_url_ref` / `load_local_postgres_url_ref`. **Hecho** — sin REQUIRED_RESOURCES literal (overkill para un solo resource), pero el discovery es por `resolve_resource(Postgres, …)` y las helpers locales fueron borradas.
- [x] VA-J'-5b. `apply_storage_config_set`: borrar todo manejo de `postgres_url` / `postgres_url_ref` (el field deja de existir en CONFIG_SET). El handler de CONFIG_SET pasa a manejar SOLO config no-secreta (si la tiene; storage hoy solo tenía postgres_url → CONFIG_SET puede quedar como inert con un mensaje "no secret-bearing fields, use vault_put"). **Hecho** — rechaza `postgres_url`/`postgres_url_ref` con `INVALID_CONFIG`.
- [x] VA-J'-5c. Boot path: `resolve_resource(Postgres, my_ilk, my_tenant)` → si lo encuentra, inicializa el Storage backend; si no, degraded (igual que hoy con `STORAGE_NOT_READY`). **Hecho** — strict rejection de URLs con dbname embebido (cada consumer agrega su dbname con `with_dbname`).
- [x] VA-J'-5d. Refresh loop cada 60s. **Hecho parcial** — `run_storage_vault_refresh_loop` probea `resolve_resource(Postgres)` cada 60s y actualiza un `AtomicBool vault_postgres_live` que el CONFIG_GET reporta en `resources.postgres.live_in_vault`. **Reporting-only**: la reconexión del pool en hot rotation sigue requiriendo `restart sy-storage` (documentado en las notas del CONFIG_GET).
- [x] VA-J'-5e. `CONFIG_GET` reporta `resources.postgres`. **Hecho**.
- [ ] VA-J'-5f. E2E: operador `vault_put` con `resource_type=postgres, tenant=sys` (en pool, sin ilk) → restart storage → reporta `resolved=true, source=pool`. **Pendiente test en VM**.

### J'-6. `sy.identity`

- [x] VA-J'-6a. `REQUIRED_RESOURCES = [(Postgres, "database.postgres_url")]`. Borrar `IDENTITY_LOCAL_REF_KEY_POSTGRES_URL` y persist/load equivalentes. **Hecho** — constante y helpers borrados.
- [x] VA-J'-6b. `apply_identity_config_set`: borrar manejo de `postgres_url_ref`. CONFIG_SET queda solo para config no-secreta (si aplica). **Hecho** — rechaza secret-bearing fields.
- [x] VA-J'-6c. Boot path para resolver Postgres: usar `resolve_resource(Postgres, my_self_ilk_deterministic, "sys")`. El operador asigna el secret a identity vía `owner_node: "SY.identity"` (que admin resuelve a `identity_ilk_deterministic`). Match directo sin SHM resolution. **Hecho** — usa `DEFAULT_ROOT_TENANT_ID` como tenant, el match sys-pool universal cubre el caso por defecto.
- [x] VA-J'-6d. Refresh loop como storage. **Hecho** — `run_identity_vault_refresh_loop` corre solo en motherbee primary, cada 60s probea `resolve_resource(Postgres)` y actualiza `vault_postgres_live` que CONFIG_GET refleja. Reporting-only; reconexión sigue requiriendo restart.
- [x] VA-J'-6e. CONFIG_GET reporta `resources.postgres`. **Hecho**.
- [ ] VA-J'-6f. E2E: operador `vault_put` con `resource_type=postgres, tenant=sys, owner_node="SY.identity"` → restart identity → DB ready. **Pendiente test en VM**.

### J'-7. `sy.admin` (executor OpenAI)

- [x] VA-J'-7a. `REQUIRED_RESOURCES = [(OpenAi, "ai_providers.openai.api_key")]`. **Hecho** — discovery directo por `resolve_resource(Openai, …)`.
- [x] VA-J'-7b. Borrar `OpenAiSection.api_key_ref` del schema de admin executor config. CONFIG_SET de admin solo persiste `default_model`, `max_tokens`, `temperature`, `top_p`, `catalog.{mode,actions}`. **Hecho**.
- [x] VA-J'-7c. `build_admin_executor_ai_runtime` ahora usa `resolve_resource(OpenAi, ...)` en vez de leer `api_key_ref` del config. **Hecho**.
- [x] VA-J'-7d. Refresh loop. **Hecho** — `run_admin_executor_vault_refresh_loop` corre cada 60s y llama `refresh_admin_executor_ai_runtime`, que ya tenía la lógica de probe + reload del runtime. Flippa `executor_configured` cuando aparece/desaparece el secret en vault.
- [x] VA-J'-7e. CONFIG_GET reporta `resources.openai`. **Hecho**.
- [ ] VA-J'-7f. E2E con `vault_put` en pool sys + restart admin → executor configured. **Pendiente test en VM**.

### J'-8. `sy.architect`

- [x] VA-J'-8a. `REQUIRED_RESOURCES = [(OpenAi, "ai_providers.openai.api_key"), (Postgres, "storage.messages_db_url")]`. **Hecho** — discovery directo por `resolve_resource` para ambos.
- [x] VA-J'-8b. Borrar `api_key_ref` y `messages_db_url_ref` del schema. Borrar `extract_*_ref`, `reject_*_plaintext`, `resolve_*_from_vault` que armé en J4. **Hecho** — reemplazado por `reject_architect_secret_fields` + `resolve_architect_openai_api_key_from_vault` / `resolve_messages_db_url_from_vault` que usan `resolve_resource`.
- [x] VA-J'-8c. `build_architect_ai_runtime` y `refresh_architect_messages_db_url` usan `resolve_resource(...)`. El messages_db se distingue del OpenAI por `resource_type=postgres`. Si hay un Postgres dedicado a architect via `ilk`, lo usa; si no, el del pool sys. **Hecho**.
- [x] VA-J'-8d. Refresh loop unificado para ambos resources. **Hecho** — `architect_secret_refresh_loop` actualizado: ahora refresca **siempre** (no solo cuando degraded) tanto `refresh_architect_ai_runtime` como `refresh_architect_messages_db_url`. Intervalo alineado a **60s** con los otros 4 consumers (storage/identity/admin/cognition) — los 10s preexistentes daban 12 round-trips/min al vault solo desde architect, lo cual era ruidoso considerando que cada tick hace 2 lookups (openai + postgres).
- [x] VA-J'-8e. CONFIG_GET reporta `resources.{openai,postgres}`. **Hecho**.
- [ ] VA-J'-8f. E2E con dos `vault_put`s (openai en pool + postgres en pool) y verificación de archi. **Pendiente test en VM**.

### J'-9. `sy.cognition`

- [x] VA-J'-9a. `REQUIRED_RESOURCES = [(OpenAi, "ai_providers.openai.api_key"), (Postgres, "storage.postgres_url")]`. **Hecho** — discovery directo por `resolve_cognition_resource(<type>, …)`.
- [x] VA-J'-9b. Borrar `COGNITION_LOCAL_REF_KEY_OPENAI`, `COGNITION_LOCAL_REF_KEY_STORAGE_POSTGRES_URL`, `extract_*_ref`, `reject_*_plaintext`, `resolve_*_to_plaintext`, `persist_local_*_ref` (todo lo de J5 legacy). **Hecho** — todas las helpers y constantes borradas, reemplazadas por `reject_cognition_secret_fields`.
- [x] VA-J'-9c. Cognition usa el MISMO secret de pool que storage para Postgres (mismo `resource_type=postgres, tenant=sys, ilk=null` en vault). Sin duplicación. **Hecho** — comparte el pool match con storage; cognition aplica `with_dbname("fluxbee_storage")` en su path.
- [x] VA-J'-9d. Refresh loop. **Hecho** — `run_vault_refresh_loop` corre cada 60s, probea `resolve_cognition_openai_api_key` y actualiza `ai_secret_source`. Esto cierra el "lazy state" donde cognition reportaba `degraded_no_ai_provider` hasta que llegara el primer turn semantic.
- [x] VA-J'-9e. CONFIG_GET con `resources.{openai,postgres}`. **Hecho**.
- [ ] VA-J'-9f. E2E: cognition cold boot sin secrets → operador hace 2 vault_puts (compartidos con storage/architect) → cognition arranca configurado. **Pendiente test en VM**.

### J'-AI. `ai-generic` + `ai-frontdesk-gov` (Phase J' migration de Phase J VA-J6 legacy)

- [x] VA-J'-AI-1. `ai-generic` (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs`): borrar las 4 fuentes alternativas (`load_local_openai_api_key`, control plane legacy, YAML inline, env var fallback) y reemplazarlas por `resolve_resource(Openai, self_tenant_id)` puro. Usa los envs `FLUXBEE_NODE_ILK_ID` + `FLUXBEE_NODE_TENANT_ID` inyectados por el orchestrator. `OpenAiApiKeySource` queda con dos variantes: `Vault` y `Missing`. **Hecho**.
- [x] VA-J'-AI-2. `ai-frontdesk-gov` (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`): mismo cambio que ai-generic, pero como es un SY system node, usa `DEFAULT_ROOT_TENANT_ID` (no FLUXBEE_NODE_TENANT_ID). `self_ilk_id` viene de `wait_for_self_system_ilk_id` (ya estaba). **Hecho**.
- [x] VA-J'-AI-3. Borrar `resolve_openai_api_key_source_from_effective_config` y tests asociados de ambos runners. **Hecho**.
- [ ] VA-J'-AI-4. E2E en VM: arrancar ai-generic sin secret en vault → confirmar `OpenAiApiKeySource::Missing` en NODE_STATUS_GET → operador hace `vault_put` con `resource_type=openai` (pool sys) → siguiente request resuelve y responde OK. **Pendiente test en VM.**

### J'-IO-slack. `io-slack` (Phase K parcial)

- [x] VA-J'-IO-slack-1. Boot lookup via `resolve_resource(Slack, self_tenant_id)`. El secret de vault es un objeto con `app_token` + `bot_token`; helper `extract_slack_tokens_from_vault_value` valida que ambos vengan no-vacíos. **Hecho**.
- [x] VA-J'-IO-slack-2. Eliminar las cadenas legacy: env vars (`SLACK_APP_TOKEN`/`SLACK_BOT_TOKEN`) + `resolve_secret(spawn_doc, ...)` para los dos tokens. **Hecho** — el bloque de carga en `Config::from_path_or_env` ahora deja ambos en `None` y delega al lookup post-boot. Las funciones `resolve_secret` / `json_get_string_opt` siguen porque otros campos no-secret todavía las usan.
- [x] VA-J'-IO-slack-3. Refresh loop cada 60s (`run_slack_vault_refresh_loop`) que reusa `resolve_slack_credentials_from_vault` + `slack.reload_credentials`. Cubre vault_put post-boot y rotaciones en runtime. **Hecho**.
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

- [ ] VA-J'-10a. Unit tests del nuevo `authorize_*` con la matriz: dedicated owner vs different owner, pool vs cross-tenant, vault_put solo admin/architect.
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

---

## 12ter. Phase K - IO nodes vault migration (deferred)

Objetivo: aplicar el mismo patrón vault-only que Phase J a los nodos IO cuando se introduzcan sus tokens / credenciales. **No es parte del ciclo actual** — queda escrito para no perderlo de vista.

Pattern esperado (espejo de Phase J):

- IO node `CONFIG_SET` acepta solo `*_ref: "vault://<key>"` para campos secretos.
- Operador setea plaintext con `vault_put`.
- Node resuelve con `resolve_vault_ref` (SDK, VA-J7c) al boot/refresh.
- Node corre degradado si vault no responde, retry según política SDK.
- Sin escritura a `secrets.json` plaintext del nodo.

### K1. `IO.api`

- [ ] VA-K1a. Identificar todos los campos secretos en config actual de `IO.api` (API keys de endpoints, webhook signing secrets, bearer tokens, etc.).
- [ ] VA-K1b. `CONFIG_SET` acepta solo `*_ref` para cada uno.
- [ ] VA-K1c. Borrar paths plaintext + escrituras a `secrets.json` del nodo.
- [ ] VA-K1d. E2E vault_put → CONFIG_SET ref → request HTTP entra al endpoint con auth correcta.

### K2. `IO.slack` — **superseded** por Phase J' (VA-J'-IO-slack)

VA-K2a..c quedaron sin sentido en Model D' (no hay `*_ref` para Slack; el secret va directo a vault como `resource_type=slack` y el nodo lo resuelve con `resolve_resource`). La migración real está cerrada en J'-IO-slack arriba.

### K3. `IO.linkedhelper` — **superseded** por Phase J' (VA-J'-IO-linkedhelper, posponido por multi-tenant)

VA-K3a..c quedaron sin sentido en Model D'. La migración real está en J'-IO-linkedhelper arriba, **posponida** porque el flujo multi-tenant (adapters[] con tenant + installation_key por entry) necesita charla de diseño antes de codear.

### K4. Borrado final de `secrets.json` plaintext en todo IO

- [ ] VA-K4a. Verificar que después de K1-K3 ningún IO node escribe plaintext a `secrets.json`.
- [ ] VA-K4b. Si el SDK siguiera teniendo write paths para IO secrets (post-J8), eliminarlos.

### K5. Out-of-scope explícito (post-Phase K)

- Encriptación de tokens IO en tránsito hacia los providers externos (Slack/LinkedHelper/etc.) — eso es responsabilidad del provider, no de fluxbee.
- Rotation automática de tokens IO con cron — defer salvo que aparezca un caso concreto.

---

## 13. Proposed first coding slice

Smallest useful implementation slice:

1. Rust SDK constants, structs, `vault_ref` parser, and basic `vault_get`/`vault_put`.
2. `sy-vault` boot with key + SQLite schema + local audit table.
3. `VAULT_PUT`, `VAULT_GET_METADATA`, `VAULT_GET`.
4. Authorization by `routing.src_l2_name`, `meta.src_ilk`, and identity SHM lookup.
5. Admin actions for those three verbs with secret redaction.
6. `SY.architect` first consumer with `vault_get_with_retry`.
7. E2E with admin put/get, architect secret load, and denial path.

Do not start with all verbs. Rotation, rollback, delete, list, Go SDK, and broad consumer migration can follow once the core boot/auth/encryption/admin/architect path is proven.

## 14. Closed implementation constants

- Alpha reset deletes vault DB and master key.
- `sy-vault` uses the same service user/ownership model as existing SY services.
- Audit is write-only from the system perspective in v1; operators inspect SQLite directly.
- SDK retry defaults: `60s` max elapsed, `250ms` initial delay, `5s` max delay, `20%` jitter.
- SDK retries only unavailable/timeout/not-found states, never auth or malformed-input errors.
