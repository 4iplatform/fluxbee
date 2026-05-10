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
- [ ] VA-A7. Define response envelopes consistently with current L2 system message style.

## 5. Phase B - Rust SDK vault contract

- [ ] VA-B1. Add `crates/fluxbee_sdk/src/vault.rs`.
- [ ] VA-B2. Define verb constants:
  - `VAULT_PUT`
  - `VAULT_GET`
  - `VAULT_LIST`
  - `VAULT_DELETE`
  - `VAULT_ROTATE`
  - `VAULT_ROLLBACK`
  - `VAULT_GET_METADATA`
- [ ] VA-B3. Define request/response structs for all verbs.
- [ ] VA-B4. Define `VaultMetadata`, `VaultFilter`, and typed error handling.
- [ ] VA-B5. Implement L2 helper functions using `NodeSender` and trace-id matching pattern used by identity helpers.
- [ ] VA-B6. Re-export vault types/helpers from `fluxbee_sdk::lib` and prelude if appropriate.
- [ ] VA-B7. Add unit tests for serialization and response parsing.
- [ ] VA-B8. Implement `vault_ref` parsing helper for `vault://<key>`.
- [ ] VA-B9. Implement `vault_get_with_retry` with bounded attempts/time, backoff, and jitter.
- [ ] VA-B10. Ensure retry does not retry `UNAUTHORIZED`, `INVALID_KEY_FORMAT`, or malformed refs.

## 6. Phase C - `sy-vault` node implementation

- [ ] VA-C1. Add `src/bin/sy_vault.rs`.
- [ ] VA-C2. Add binary target `sy-vault` to `Cargo.toml`.
- [ ] VA-C3. Load hive id and node name using the same core service pattern as other SY nodes.
- [ ] VA-C4. Implement lock file `/var/run/fluxbee/sy-vault.lock`.
- [ ] VA-C5. Implement master key load/generate:
  - create if missing;
  - validate exact 32 bytes;
  - enforce `0600` or stricter;
  - fail closed on invalid key.
- [ ] VA-C6. Implement SQLite schema creation and schema version check.
- [ ] VA-C7. Implement AES-256-GCM encrypt/decrypt.
- [ ] VA-C8. Implement `VAULT_PUT`.
- [ ] VA-C9. Implement `VAULT_GET`.
- [ ] VA-C10. Implement `VAULT_LIST`.
- [ ] VA-C11. Implement `VAULT_DELETE`.
- [ ] VA-C12. Implement `VAULT_ROTATE`.
- [ ] VA-C13. Implement `VAULT_ROLLBACK`.
- [ ] VA-C14. Implement `VAULT_GET_METADATA`.
- [ ] VA-C15. Ensure secret values are never logged.
- [ ] VA-C16. Add structured audit write for every operation, including denial and no-op.
- [ ] VA-C17. Update `last_accessed_at` and `access_count` only after successful authorized GET.
- [ ] VA-C18. Add graceful shutdown.
- [ ] VA-C19. Ensure audit writes are in the same SQLite transaction as secret mutation when applicable.

## 7. Phase D - Authorization

- [ ] VA-D1. Implement caller extraction from current message shape:
  - `routing.src_l2_name`
  - `meta.src_ilk`
  - `routing.trace_id`
- [ ] VA-D2. Resolve caller tenant and ILK type through identity SHM by canonical `meta.src_ilk`.
- [ ] VA-D3. Implement admin override for `SY.admin@*` and `SY.architect@*`.
- [ ] VA-D4. Implement owner authorization: caller `meta.src_ilk == metadata.owner_ilk`.
- [ ] VA-D5. Implement same-tenant system authorization if caller is `system` ILK.
- [ ] VA-D6. For `tenant_id = sys`, allow only admin/architect.
- [ ] VA-D7. Prevent key enumeration for `VAULT_GET` and `VAULT_LIST`.
- [ ] VA-D8. Unit-test allowed/denied matrix.

## 8. Phase E - Admin and Archi integration

- [ ] VA-E1. Add admin action handlers that proxy to `SY.vault` over L2.
- [ ] VA-E2. Add action help entries for:
  - `vault_put`
  - `vault_get`
  - `vault_list`
  - `vault_delete`
  - `vault_rotate`
  - `vault_rollback`
  - `vault_get_metadata`
- [ ] VA-E3. Mark value-bearing actions as secret-sensitive in previews/logs.
- [ ] VA-E4. Ensure executor plan display redacts `value`.
- [ ] VA-E5. Add SCMD examples to admin help reference.
- [ ] VA-E6. Add Archi handbook guidance at method level only:
  - discover action help;
  - do not print secret values;
  - prefer metadata/list operations for inspection.
- [ ] VA-E7. Change node `CONFIG_SET` secret handling path: admin writes secret-bearing values to vault and forwards/stores only `vault://...` refs.
- [ ] VA-E8. Use node `CONFIG_GET contract.secrets[*]` as the source of truth for secret-bearing fields.
- [ ] VA-E9. Keep admin action/result payloads redacted even when the operator supplied plaintext in the request.

## 9. Phase F - Install, orchestrator, and core manifest

- [ ] VA-F1. Add `sy-vault` compile/install path in `scripts/install.sh`.
- [ ] VA-F2. Install `/usr/bin/sy-vault`.
- [ ] VA-F3. Add `sy-vault` to `/var/lib/fluxbee/dist/core/bin`.
- [ ] VA-F4. Add `sy-vault` to core manifest.
- [ ] VA-F5. Add systemd unit for `sy-vault`.
- [x] VA-F6. Decide whether orchestrator should start it by default: yes, normal SY service.
- [ ] VA-F7. Add to orchestrator-managed service set without making AI/IO/SY node startup block on vault readiness.
- [ ] VA-F8. Add clean/reset behavior to `fluxbee_cleanall.sh`:
  - alpha clean deletes vault DB/key;
  - no extra confirmation in this development cycle.
- [ ] VA-F9. Add file permission/ownership checks in install or vault boot.
- [x] VA-F10. Decide service user model: same as existing SY services; no dedicated `sy-vault` user in v1.

## 10. Phase G - Consumer migration strategy

- [ ] VA-G1. Define `vault_ref` convention for node config:
  - example: `config.secrets.openai.api_key_ref = "vault://sys:openai-api-key"`.
- [ ] VA-G2. Deprecate new node-local plaintext `secrets.json` writes once vault is enabled.
- [ ] VA-G3. Implement `SY.architect` as the first vault-backed consumer:
  - starts without OpenAI key;
  - reports missing secret;
  - retries/loads via SDK helper when key appears;
  - transitions to configured without special orchestrator ordering.
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
