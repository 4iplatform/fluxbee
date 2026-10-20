# IO Slack Deploy Runbook (Fluxbee-compliant path)

## 1) Goal

This runbook describes how to deploy and operate `IO.slack` through SY.admin, the path every
managed node takes (`docs/reference/operations.md`, "Publish runtimes" and "Node lifecycle"):

1. publish the runtime in `dist/`, only for a build the package does not carry (the `.deb` seeds
   `io.slack` from `packaging/base-nodes.json`)
2. `POST /hives/{hive}/update` (`SYSTEM_UPDATE`, `category=runtime`) on the target hive
3. `POST /hives/{hive}/nodes` (`run_node`), or `POST /hives/{hive}/nodes/{node}/restart` for a
   node that already exists
4. the Slack tokens in SY.vault, and the node's config through the control plane (`CONFIG_GET` /
   `CONFIG_SET`)

There is no direct local path for `IO.slack`: the supported deploy is a published
runtime + an instance managed by the orchestrator.

---

## 2) Prerequisites

- `sy-orchestrator`, `sy-admin`, `rt-gateway` active on the hive.
- Admin API available (`BASE`, for example `http://127.0.0.1:8080`).
- An active tenant for the binding: an IO.slack node never runs in the root tenant
  (`TENANT_ROOT_NOT_ALLOWED`).
- For a new build: `io-slack` buildable and `scripts/publish-io-runtime.sh`, run on the motherbee.

Suggested variables:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
RUNTIME="io.slack"
VERSION="0.1.0"
NODE_NAME="IO.slack.T123@$HIVE_ID"
TENANT_ID="tnt:<uuid>"                 # the tenant the binding works for
VAULT_KEY="slack:io-slack-t123"        # SY.vault key: [a-z0-9:_-], first character [a-z0-9]
```

Current spawn contract in `io-slack`:

- requires `FLUXBEE_NODE_NAME`, provided by the platform
- uses the canonical managed path for `config.json`
- does not use a legacy spawn fallback/env

---

## 3) Initial deploy (compliant)

With the version the package seeded, skip steps 1 to 3.

### Step 1: publish runtime

```bash
bash scripts/publish-io-runtime.sh --kind slack --version "$VERSION" --set-current --sudo
```

Save the output:

```bash
MANIFEST_VERSION=<manifest_version>
MANIFEST_HASH=<manifest_hash>
```

### Step 2: (optional, recommended) sync-hint dist

A worker pulls the new package after the motherbee's Syncthing scans it; a scan on the worker
pulls nothing. Scan on the motherbee:

```bash
curl -sS -X POST "$BASE/hives/motherbee/sync-hint" \
  -H "Content-Type: application/json" \
  -d '{"channel":"dist","folder_id":"fluxbee-dist-runtimes","wait_for_idle":true}'
```

### Step 3: SYSTEM_UPDATE runtime

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
  -H "Content-Type: application/json" \
  -d "{
    \"category\":\"runtime\",
    \"manifest_version\": $MANIFEST_VERSION,
    \"manifest_hash\": \"$MANIFEST_HASH\",
    \"runtime\": \"$RUNTIME\",
    \"runtime_version\": \"$VERSION\"
  }"
```

- Repeat while it answers `sync_pending` (HTTP 202); go on at `status=ok`.
- With `runtime` and `runtime_version`, only this runtime's artifacts must be present on the hive.
  Without them every runtime of the manifest must be, and one that is missing keeps the answer at
  `sync_pending` even though `io.slack` itself is in place.
- The update restarts no node.

### Step 4: launch the node (`run_node`)

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"current\",
    \"tenant_id\":\"$TENANT_ID\",
    \"config\":{
      \"slack\":{\"auth\":{\"type\":\"vault_ref\",\"resource_type\":\"slack\",\"key\":\"$VAULT_KEY\"}},
      \"io\":{\"workspace_id\":\"T123\",\"conversation_id\":\"C456\"}
    }
  }"
```

Notes:

- `tenant_id` goes at the top level. The orchestrator registers the node's ILK and launches it.
- `config` is the object `CONFIG_SET` takes (§5). `slack.auth` names the vault key, never the
  tokens: inline tokens are refused. `io.workspace_id` and `io.conversation_id` are required;
  `io.dst_node`, `io.relay.*`, `identity.target` and `identity.timeout_ms` are optional.
- An existing node answers `NODE_ALREADY_EXISTS`: restart it instead (§4).

### Step 5: the Slack tokens in SY.vault

```bash
curl -sS -X POST "$BASE/hives/motherbee/vault/secrets" \
  -H "Content-Type: application/json" \
  -d "{
    \"key\":\"$VAULT_KEY\",
    \"value\":{\"app_token\":\"xapp-...\",\"bot_token\":\"xoxb-...\"},
    \"metadata\":{\"tenant_id\":\"$TENANT_ID\",\"resource_type\":\"slack\",\"owner_node\":\"$NODE_NAME\"}
  }"
```

- SY.vault lives on the motherbee: the call goes to `/hives/motherbee/...` wherever the node runs.
- `owner_node` makes the secret the node's own. SY.admin resolves it to the node's ILK, so the node
  must exist first (step 4). Without `owner_node` the secret is general: every node of the tenant
  can read it.
- The node reads the key at start, on a `VAULT_SECRET_CHANGED` for it, and every 60 s; until it
  has the tokens it runs degraded.

---

## 4) Update code (new build)

For code changes, the right flow is not to overwrite `/usr/bin` by hand.

1. Publish the new version and update the hive (§3, steps 1 to 3, with the new version).
2. Restart the node through SY.admin:

   ```bash
   curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/restart"
   ```

   The restart recreates the node's unit at the version now `current`, for a node launched with
   `runtime_version: current`; a node pinned to a version keeps it. Its config, ILK and vault
   secret stay.

To start over instead, remove the node:

```bash
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" \
  -H "Content-Type: application/json" -d '{"purge_instance":true}'
```

That is the full teardown: the node's directory, its ILK and the vault secrets it owns go too, so
a new node needs §3 steps 4 and 5 again. Without `purge_instance` the `DELETE` only stops the node
and keeps it down; `POST .../nodes/<node>/start` brings it back with the same config.

---

## 5) Update only the configuration (no binary change)

For a hot reload of the functional configuration of `IO.slack`, the canonical path is the node's control-plane:

1. `CONFIG_GET` to see the state/effective config, redacted.
2. `CONFIG_SET` with a monotonic `config_version` and `apply_mode=replace`.
3. check `CONFIG_RESPONSE` (`ok/error`) and the `apply` fields.

`CONFIG_GET` example:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"archi"}'
```

`CONFIG_SET` example:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version":1,
    "config_version":8,
    "apply_mode":"replace",
    "config":{
      "io":{
        "workspace_id":"T123",
        "conversation_id":"C456",
        "dst_node":"AI.chat@motherbee",
        "relay":{
          "window_ms":2500,
          "max_open_sessions":10000,
          "max_fragments_per_session":8,
          "max_bytes_per_session":262144
        }
      },
      "slack":{"auth":{"type":"vault_ref","resource_type":"slack","key":"slack:io-slack-t123"}}
    }
  }'
```

`replace` replaces the whole config, so the payload carries the required fields too
(`slack.auth`, `io.workspace_id`, `io.conversation_id`).

How to read `apply` in `CONFIG_RESPONSE`:

- `hot_applied`: changes applied hot (no process restart)
- `reinit_performed`: internal reinitialization of the adapter performed
- `restart_required`: changes that need a process restart to take full effect

Important:

- `PUT /hives/{hive}/nodes/{node}/config` (core) persists the managed `config.json`, but it does not replace `CONFIG_SET` as the runtime hot-reload contract in IO.
- `CONFIG_CHANGED` is treated as an informational signal in IO v1.

---

## 6) Recovery from `FAILED_CONFIG`

Typical case: the node started with a minimal bootstrap or an invalid config and was left in a non-operational state.

Recovery sequence:

1. send `CONFIG_GET` and confirm the state + last error.
2. fix the payload and send `CONFIG_SET` with a higher version.
3. validate `CONFIG_RESPONSE status=ok` and the state transition.
4. run a real functional test (inbound/outbound).

If `CONFIG_SET` responds with an error:

- `invalid_config`: check the shape and the mandatory fields of the Slack adapter.
- `stale_config_version`: resend with a `config_version` higher than the last one applied.
- `config_persist_error`: check the permissions/path for persisting the dynamic state.

---

## 7) Post-deploy validation

Minimum:

- the node's `status` in the admin API
- the runtime logs on the hive
- a real test of inbound `app_mention` and outbound `chat.postMessage`

Optional:

- `CONFIG_GET` and check `metrics.control_plane`
- `GET /hives/{id}/versions` and check the readiness of `IO.slack` at the current version

### Relay logs

The inbound relay of `IO.slack` lives in `io-common`, and its current logging is event-driven.

What to expect:

- `info`: `io-slack relay policy initialized`
- `debug`: `relay session opened`, `relay fragment buffered`, `relay session flushed`, `relay duplicate fragment dropped`, `relay passthrough immediate`
- `warn`: `relay capacity rejected`, drops because of expiry/limits, and fail-open fallback

How to see it in the canonical runtime:

- the runtime published by `scripts/publish-io-runtime.sh` already includes a default `RUST_LOG` with `io_common=debug`
- so, in a canonical deploy, those events should show up in `journalctl`

---

## 8) `text/v1` contract note (offload > limit)

In the current state, the size-based offload rule for `text/v1` is centralized in `fluxbee_sdk::NodeSender::send`:

- `io-slack` builds the base payload (`content` + `attachments`).
- `NodeSender::send` decides inline vs `content_ref` using the SDK's internal defaults (`max_message_bytes=64KB`, `message_overhead_bytes=2048`).

Expected logs:

- `sdk send text/v1 normalized` (debug)
- `sdk send text/v1 processed as blob` (info, only when there was an offload)

This avoids divergence between IO adapters and keeps `text/v1` behavior uniform.

Important:
- `io.blob.*` configured through the node's control-plane does not apply automatically to the `NodeSender::send` enforcement at this stage.

Note on test scope:

- To validate very large text payloads deterministically (for example `>64KB`), use `io-sim`.
- In real Slack, `chat.postMessage` has the channel's own limits and truncates text above 40,000 characters, so it is not a reliable channel to test extreme internal thresholds.
  - Source: https://docs.slack.dev/reference/methods/chat.postMessage/ ("Truncating content").

Current outbound state to keep in mind in tests:

- If `io-slack` receives long text through `content_ref`, today it resolves it and posts it as text through `chat.postMessage`.
- So very long texts can end up truncated by Slack's limit.
- Validation of the canonical fallback (file/snippet/chunking) is pending a channel-specific rule.

## 9) Operational note on inbound attachments

In the current state, `IO.slack` does not accept just any file Slack delivers.

The inbound normalization to `text/v1.attachments[]` goes through the common `io-common` validation, but the effective allowlist is defined by `IO.slack`. In the current state, `IO.slack` accepts:

- `image/jpeg`
- `image/png`
- `image/webp`
- `image/gif`
- `application/pdf`
- `text/plain`
- `text/markdown`
- `application/json`
- `text/csv`
- `application/msword`
- `application/vnd.ms-excel`
- `application/vnd.ms-powerpoint`
- `application/vnd.openxmlformats-officedocument.wordprocessingml.document`
- `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`
- `application/vnd.openxmlformats-officedocument.presentationml.presentation`

Consequence:

- `xlsx`, `docx`, `csv` and equivalent common office files should reach the router and `AI.*` nodes if Slack delivers them with those expected MIME types
- if a file is outside the effective allowlist, it is dropped inbound with `unsupported_attachment_mime`
- so the real E2E limit is the effective policy of `IO.slack`, not `AI`, for any type the adapter does not accept yet

Scope of this note:

- this does not mean that `IO.slack` accepts every format Slack can carry
- it only records the set the adapter explicitly accepts today in this repo
- any new type needs to be added explicitly to the effective policy and validated E2E

To adjust the accepted set at deploy time, `IO.slack` supports an override through:

- `IO_SLACK_ALLOWED_MIMES`
- `io.blob.allowed_mimes_csv`
