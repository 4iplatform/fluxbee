# Node Secret Config Spec

Status: working draft  
Scope: non-`SY` nodes (`AI.*`, `IO.*`, `WF.*`) with optional local reuse by `SY.architect` for its own private secrets

## 1. Goal

Define one simple, standard way for nodes to receive and persist secrets such as API keys without storing them in `hive.yaml`.

This spec is intentionally minimal:

- secrets arrive only through `archi` commands / `SCMD`
- transport uses the existing L2/socket control plane
- secrets are stored locally on each node
- no mandatory encryption-at-rest is introduced in v1
- no global vault is introduced here

## 2. Non-goals

This spec does not attempt to solve:

- global secret management
- secret replication across hives
- secret escrow or recovery
- custom encryption schemes
- replacing the current orchestrator-owned `config.json` / `CONFIG_CHANGED` flow

The current per-node orchestrator config path remains unchanged and separate.

## 3. Design principles

- Secret transport is initiated only from `archi`.
- `SY.admin` acts only as the control-plane gateway and transport wrapper.
- Node-specific secret semantics remain owned by each node implementation.
- The platform standardizes only the storage rules, metadata expectations, and redaction behavior.
- Plain secret values must never be returned by read APIs, status payloads, or logs.

## 4. Relationship with `CONFIG_GET` / `CONFIG_SET`

This spec reuses the node config control plane already defined in:

- `docs/node-config-control-plane-spec.md`

No new business verbs are introduced.

The model is:

- `CONFIG_GET`
  - used to discover the node contract
  - must describe which fields are secrets
  - must not return raw secret values
- `CONFIG_SET`
  - carries node-defined configuration, including secret fields when needed
  - is sent unicast over L2 to the target node
- `CONFIG_RESPONSE`
  - returns apply/read result
  - must keep secret values redacted

Important:

- the common platform still does not define the node-specific shape of `payload.config`
- each node family defines its own secret-bearing fields
- the node contract returned by `CONFIG_GET` must tell `archi` which fields are secrets, required, optional, and currently configured

## 5. End-to-end flow

Canonical flow:

1. Operator sends a command to `archi`.
2. `archi` translates it into `SCMD` / admin control usage.
3. `SY.admin` sends a unicast L2 `CONFIG_SET` to the target node.
4. The target node validates the payload according to its own contract.
5. The target node persists secret material into a local secret file.
6. The target node replies with `CONFIG_RESPONSE`.
7. Any later `CONFIG_GET` returns redacted state and contract metadata, not plaintext values.

This keeps the operator-facing entrypoint stable:

- command goes into `archi`
- transport goes through admin/router/socket
- persistence happens only on the target node

## 6. Storage model

Each node that supports secrets should persist them in a dedicated local file separate from:

- `config.json`
- `state.json`

Recommended path:

- `/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/secrets.json`

Examples:

- `/var/lib/fluxbee/nodes/AI/AI.chat@motherbee/secrets.json`
- `/var/lib/fluxbee/nodes/IO/IO.slack.T123@motherbee/secrets.json`

For `SY.architect` local bootstrap, the same file model may be reused under its own node directory, but this spec does not extend `CONFIG_GET` / `CONFIG_SET` to the rest of `SY.*`.

## 7. File permissions and write rules

Required baseline:

- node directory: `0700`
- `secrets.json`: `0600`
- writes must be atomic (`tmp` + rename)
- partial writes must never leave a corrupted file behind

Recommended ownership rule:

- the node process is the writer of `secrets.json`
- orchestrator remains the writer of `config.json`
- node never writes `config.json` just to persist secrets

## 8. Secret file format

Minimal recommended shape:

```json
{
  "schema_version": 1,
  "updated_at": "2026-03-27T21:00:00Z",
  "updated_by_ilk": "ilk:1234...",
  "updated_by_label": "archi",
  "trace_id": "9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0002",
  "secrets": {
    "openai_api_key": "sk-...",
    "slack_bot_token": "xoxb-..."
  }
}
```

Notes:

- `secrets` keys are node-defined
- the SDK may standardize the container shape
- the SDK must not attempt to understand business meaning of each secret
- `updated_by_ilk` is preferred over a fixed string because it preserves actor identity
- `trace_id` should be preserved from the originating admin/control request when available

## 9. `CONFIG_GET` response requirements for secrets

When a node supports secret-bearing config, `CONFIG_GET` should describe them explicitly in the returned contract.

Recommended contract fragment:

```json
{
  "contract": {
    "secrets": [
      {
        "field": "behavior.openai.api_key",
        "storage_key": "openai_api_key",
        "required": true,
        "configured": true,
        "value_redacted": true,
        "persistence": "local_file"
      }
    ]
  }
}
```

Rules:

- `configured` may be exposed
- `required` may be exposed
- `storage_key` may be exposed
- raw secret values must not be exposed
- `persistence` should be `local_file` in v1
- `value_redacted` should always be `true` for exposed secret metadata

Shared SDK support now exists for this metadata shape:

- `fluxbee_sdk::node_secret::NodeSecretDescriptor`

If the node wants to surface current config, it should either:

- omit secret values entirely, or
- return placeholders such as `***REDACTED***`

## 10. Logging and observability rules

Plain secret values must not appear in:

- admin logs
- node logs
- status payloads
- `CONFIG_RESPONSE`
- persisted message history

At most, the platform may log:

- secret field names
- whether a secret was provided
- whether apply succeeded
- which node was targeted

## 11. SDK responsibilities

This mechanism should be standardized in `crates/fluxbee_sdk`.

Recommended helper scope:

- canonical secret file path resolution
- directory/file permission enforcement
- atomic read/write helpers
- JSON load/save helpers
- redaction helpers for responses and logs
- helpers for secret metadata descriptors returned by `CONFIG_GET`

The SDK should standardize the mechanics, not the business contract.

The SDK should not define:

- which secrets a node requires
- how a node validates an OpenAI key, Slack token, or other credential
- how a node uses a secret at runtime

## 12. Optional future hardening

Plaintext + local permissions is the baseline for v1.

An optional v1.1 hardening may be added later in the SDK:

- encrypt `secrets.json` at rest with a locally derived key
- derivation inputs may include `hive_id`, `node_name`, and a local hive salt
- the SDK would make this transparent to the node implementation

This is intentionally not part of the required v1 contract.

Rationale:

- it improves protection against accidental exposure
- it improves backup hygiene
- it does not replace a vault or KMS
- it does not protect against an actor with effective machine-level access

If implemented later, it should be documented as tactical hardening, not as a full secret-management solution.

## 13. What stays node-owned

Each node or node family remains responsible for:

- defining which config fields are secrets
- defining which secret keys are required
- mapping `payload.config` fields into local `secrets.json`
- validating secret format if needed
- deciding whether secret updates are hot-reloaded or require restart

## 14. Why this is safer than `hive.yaml`

This proposal improves the current situation because:

- secrets are removed from shared infra bootstrap files
- secret persistence becomes node-local instead of hive-global
- read flows can uniformly redact values
- the platform gets one standard path instead of ad-hoc secret handling

It is still a pragmatic step, not a final security architecture.

## 15. Initial implementation plan

Suggested rollout:

1. Add local secret-file helpers to `fluxbee_sdk`.
2. Keep transport through the existing `CONFIG_SET` path via `archi` -> `SY.admin` -> L2 unicast.
3. Update node contracts so `CONFIG_GET` advertises secret fields and redaction behavior.
4. Migrate sensitive values out of `hive.yaml` for nodes that already support `CONFIG_SET`.
5. Optionally reuse the same local file model for `SY.architect` bootstrap secrets.

## 16. Implementation checklist

- [ ] SDK: add canonical secret path helper for node-local storage.
- [ ] SDK: add `0700` directory creation helper for node secret directories.
- [ ] SDK: add `0600` secret file creation/update helper.
- [ ] SDK: add atomic write helper (`tmp` + rename) for `secrets.json`.
- [ ] SDK: add JSON load/save helpers for the standard secret container.
- [ ] SDK: add redaction helper for secret-bearing config payloads and responses.
- [ ] SDK: define the standard metadata fields `updated_by_ilk`, `updated_by_label`, and `trace_id`.
- [ ] Admin/Archi docs: document that secret-bearing node config must enter only through `archi` / `SCMD`.
- [ ] Node contract docs: require `CONFIG_GET` to expose `contract.secrets[*]` with `required`, `configured`, `value_redacted`, and `persistence`.
- [ ] AI node: move OpenAI key handling out of `hive.yaml` and into local secret-file persistence.
- [ ] IO/WF follow-up: identify which nodes need local secret persistence and document their secret fields.
- [ ] Logging review: ensure admin and node logs never print raw secret values.
- [ ] State/config review: ensure secrets are not copied into `config.json`, `state.json`, or message history.
- [ ] Decide whether optional v1.1 at-rest encryption is worth implementing after v1 is stable.

## 17. Open questions

- Should the SDK expose a default redaction token such as `***REDACTED***`?
- Should the SDK provide a small helper for `configured=true/false` contract entries?
- Should `updated_by_label` always be optional if `updated_by_ilk` is already present?
