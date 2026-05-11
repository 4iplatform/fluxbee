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
- [ ] VA-G2. Deprecate new node-local plaintext `secrets.json` writes once vault is enabled.
- [x] VA-G3. Implement `SY.architect` as the first vault-backed consumer:
  - starts without OpenAI key;
  - reports missing secret;
  - retries/loads via SDK helper when key appears;
  - transitions to configured without special orchestrator ordering.
  - exposes the same vault-backed OpenAI config contract through node L2 `CONFIG_GET` / `CONFIG_SET`, so admin `node_control_config_get/set` is the canonical control path.
  - orchestrator starts configured `system_nodes` from `hive.yaml` with `SY.identity` first; `sy.identity` seeds deterministic `system` ILKs from the same list, so vault consumers must have real `ilk:<uuid>` ownership instead of placeholder/workaround metadata.
  - the `hive.yaml` schema uses the compact `system_nodes.<role>.{nodes, wait_for}` shape (service/exec/start/critical are derived/defaulted in code, not declared per entry).
  - each Rust SY (`sy_admin`, `sy_architect`, `sy_cognition`, `sy_config_routes`, `sy_identity`, `sy_policy`, `sy_storage`, `sy_vault`) resolves its own `ilk:<uuid>` at boot via `fluxbee_sdk::identity::wait_for_self_system_ilk_id(...)` and caches it for the process lifetime, so outgoing vault calls can set `meta.src_ilk` directly without an extra identity round-trip. Go-side SYs (`sy-timer`, `sy-wf-rules`, `sy-opa-rules`) gain the equivalent lookup as a follow-up.
- [ ] VA-G4. Migrate `ai.generic` OpenAI key to vault refs.
- [ ] VA-G5. Migrate IO secret-bearing configs to vault refs:
  - `IO.api` API keys/webhook secrets;
  - `IO.slack` app/bot tokens;
  - `IO.linkedhelper` adapter credentials where applicable.
- [ ] VA-G6. Migrate SY DB secrets if still required:
  - `SY.storage` Postgres URL;
  - `SY.identity` Postgres URL;
  - Archi messages DB URL.
- [ ] VA-G7. Remove or mark legacy `secrets.json` write paths after consumers are migrated.
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
