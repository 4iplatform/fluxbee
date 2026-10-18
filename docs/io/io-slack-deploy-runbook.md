# IO Slack Deploy Runbook (Fluxbee-compliant path)

## 1) Goal

This runbook describes how to deploy and operate `IO.slack` through the canonical Fluxbee path:

1. publish the runtime in `dist/`
2. run `SYSTEM_UPDATE` on the target hive
3. `SPAWN_NODE` (or a controlled restart of the node)
4. configure the node's runtime through the control-plane (`CONFIG_GET` / `CONFIG_SET`)

There is no direct local path for `IO.slack`: the supported deploy is a published
runtime + an instance managed by the orchestrator.

---

## 2) Prerequisites

- `sy-orchestrator`, `sy-admin`, `rt-gateway` active on the hive.
- Admin API available (`BASE`, for example `http://127.0.0.1:8080`).
- `io-slack` binary built or buildable.
- `scripts/publish-io-runtime.sh` available.

Suggested variables:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
RUNTIME="io.slack"
VERSION="0.1.0"
NODE_NAME="IO.slack.T123@$HIVE_ID"
```

Current spawn contract in `io-slack`:

- requires `FLUXBEE_NODE_NAME`, provided by the platform
- uses the canonical managed path for `config.json`
- does not use a legacy spawn fallback/env

---

## 3) Initial deploy (compliant)

### Step 1: publish runtime

```bash
bash scripts/publish-io-runtime.sh --kind slack --version "$VERSION" --set-current --sudo
```

Save the output:

- `manifest_version=<...>`
- `manifest_hash=<...>`

### Step 2: (optional, recommended) sync-hint dist

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
  -H "Content-Type: application/json" \
  -d '{"channel":"dist","wait_for_idle":true,"timeout_ms":30000}'
```

### Step 3: SYSTEM_UPDATE runtime

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
  -H "Content-Type: application/json" \
  -d "{
    \"category\":\"runtime\",
    \"manifest_version\": $MANIFEST_VERSION,
    \"manifest_hash\": \"$MANIFEST_HASH\"
  }"
```

It must respond `status=ok` to continue.

### Step 4: SPAWN_NODE of IO.slack

Minimal example with a basic bootstrap:

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"current\",
    \"tenant_id\":\"<TENANT_ID>\",
    \"config\":{
      \"tenant_id\":\"<TENANT_ID>\"
    }
  }"
```

Notes:

- On hives with mandatory identity registration at spawn, `tenant_id` is required.
- The node can start without working tokens/prompt and stay non-operational (`FAILED_CONFIG` or equivalent) until a valid `CONFIG_SET`.

---

## 4) Update code (new build)

For code changes, the right flow is not to overwrite `/usr/bin` by hand.

### Recommended sequence

1. Publish a new runtime version:

```bash
NEW_VERSION="0.1.1"
bash scripts/publish-io-runtime.sh --kind slack --version "$NEW_VERSION" --set-current --sudo
```

2. Run `SYSTEM_UPDATE` with the new `manifest_version/hash`.
3. Restart the node's process so it picks up the new runtime:
   - option A: `KILL_NODE` + `SPAWN_NODE` (recommended, explicit)
   - option B: if your admin layer exposes a direct restart, use the node restart

Automated shortcut (recommended):

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$NEW_VERSION" \
  --node-name "$NODE_NAME" \
  --update-existing \
  --sync-hint \
  --sudo
```

`--update-existing` does:

- publish the new runtime
- update the runtime, with retries on `sync_pending`
- GET the node's current config
- kill-first + spawn reusing that config

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
        "dst_node":"AI.chat@motherbee",
        "relay":{
          "window_ms":2500,
          "max_open_sessions":10000,
          "max_fragments_per_session":8,
          "max_bytes_per_session":262144
        }
      },
      "slack":{"app_token_ref":"env:SLACK_APP_TOKEN","bot_token_ref":"env:SLACK_BOT_TOKEN"}
    }
  }'
```

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

## 8) Automated script (publish + update + optional spawn)

Added:

- `scripts/deploy-io-slack.sh`

Example (deploy + spawn):

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "0.1.0" \
  --node-name "IO.slack.T123@$HIVE_ID" \
  --tenant-id "<TENANT_ID>" \
  --app-token "xapp-REPLACE_ME" \
  --bot-token "xoxb-REPLACE_ME" \
  --identity-target "SY.identity@$HIVE_ID" \
  --identity-timeout-ms 10000 \
  --sync-hint \
  --kill-first \
  --sudo
```

The script:

- parses `manifest_version`/`manifest_hash` from publish,
- retries `update` if it receives `sync_pending`,
- logs each step to a file (`/tmp/deploy-io-slack-<ts>.log` by default),
- supports `--update-existing` to update the code of an existing node, reusing its config.
- optionally allows continuing even with `sync_pending` through `--allow-sync-pending` (only for test environments with an inconsistent global manifest).

### Note on a permanent `sync_pending`

If `update` stays in `sync_pending` because other runtimes of the manifest are missing (for example `wf.node.status.*.diag`), the `IO.slack` deploy can get blocked even though `IO.slack` was published correctly.

In test environments you can use:

```bash
bash scripts/deploy-io-slack.sh ... --allow-sync-pending
```

This lets you continue to `spawn`, but it does not fix the underlying problem of the global manifest.

---

## 10) `text/v1` contract note (offload > limit)

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

## 11) Operational note on inbound attachments

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
