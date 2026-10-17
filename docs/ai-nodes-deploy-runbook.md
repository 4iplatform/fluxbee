# AI nodes runbook (`ai.generic`)

## 1) The model

- **`ai.generic` is the only AI runtime.** The `.deb` bakes it
  (`/var/lib/fluxbee/dist/runtimes/ai.generic/<version>`), and every install registers that version as
  `current`.
- **An AI node (`AI.<name>@<hive>`) is an instance of `ai.generic`:**
  - `run_node` creates it, for a tenant;
  - `CONFIG_SET` gives it its behavior;
  - `set_ilk_definition` on its ILK gives it its role, skills, handbooks and personality.
- **No instance starts with the base install.** Instances are created for something concrete; test
  ones are ephemeral and do not stay in the base.
- **`SY.frontdesk.gov` is not a runtime** and is not deployed with this runbook. It is a core system
  node (`sy-frontdesk-gov.service`), updated with the `.deb`.

Variables used in the examples:

```bash
BASE="http://127.0.0.1:8080"           # SY.admin on the motherbee
HIVE_ID="motherbee"                    # the node's hive: motherbee or worker1
NODE="AI.sales@motherbee"
TENANT_ID="tnt:<uuid>"                 # the tenant the node works for
```

SY.vault and the identity writes live on the motherbee, so their calls below always use
`/hives/motherbee/...`, wherever the node runs.

---

## 2) Prerequisite: the provider's key

- **The provider is the hive's:** `ai.default_provider` in `/etc/fluxbee/hive.yaml`, or `openai` when
  there is no `ai:` section. When the section exists, it must name the provider's model
  (`ai.providers.<provider>.model`).
- **The node reads its key on every request,** from SY.vault, by the provider's `resource_type`
  (`openai` or `anthropic`), in its own tenant:
  - a key the node owns;
  - else a key assigned to it;
  - else the tenant's general key (no owner, no assigned node).

  The key's name does not matter.
- **A tenant's general key** is loaded once with that tenant's `tenant_id` and no owner:

  ```bash
  curl -sS -X POST "$BASE/hives/motherbee/vault/secrets" -H 'content-type: application/json' \
    -d '{"key":"ai:openai:general","value":{"api_key":"sk-..."},"metadata":{"tenant_id":"'"$TENANT_ID"'","resource_type":"openai"}}'
  ```

- **The root tenant's key is the core's.** A key loaded without `metadata.tenant_id` goes to the root
  tenant. That one is read by SY.architect, SY.admin's executor, SY.cognition and SY.frontdesk.gov,
  never by a tenant's AI node (`docs/ilk-tenant-model-v1.md` §2.2).
- **SY.vault hands out a tenant's key only while the tenant is active.**
- **The node names no key.** A config with `behavior.vault_key` is refused.
- **Without a usable key**, the node answers each message with a `missing_ai_api_key` error.

---

## 3) Create a node

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" -H 'content-type: application/json' \
  -d '{"node_name":"'"$NODE"'","runtime":"ai.generic","runtime_version":"current","tenant_id":"'"$TENANT_ID"'"}'
```

- **`tenant_id` goes at the top level.**
  - Without a tenant the spawn fails (`IDENTITY_REGISTER_FAILED`).
  - The tenant must be active (`TENANT_NOT_ACTIVE` otherwise).
  - It must not be the root tenant (`TENANT_ROOT_NOT_ALLOWED`).
- **The orchestrator registers the node's ILK** (an `agent` ILK) and launches the node with
  `FLUXBEE_NODE_ILK_ID` and `FLUXBEE_NODE_TENANT_ID`. To find it later, list
  `GET $BASE/hives/motherbee/identity/ilks` and take the entry whose `node_name` is the node.
- **The node starts UNCONFIGURED** and answers `node_not_configured` until it gets its `CONFIG_SET`.
  `run_node` also takes a `config`, the same object `CONFIG_SET` takes; with it the node starts
  configured.

---

## 4) Configure it (`CONFIG_SET`)

1. `POST .../nodes/<node>/control/config-get` and read `payload.response.config_version`.
2. `POST .../nodes/<node>/control/config-set` with `config_version + 1`.

```bash
CURRENT=$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE/control/config-get" \
  -H 'content-type: application/json' -d '{}' | jq -r '.payload.response.config_version')

curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE/control/config-set" \
  -H 'content-type: application/json' -d @- <<EOF
{
  "node_name": "$NODE",
  "subsystem": "ai_node",
  "schema_version": 1,
  "config_version": $((CURRENT + 1)),
  "apply_mode": "replace",
  "config": {
    "behavior": {
      "kind": "ai_chat",
      "model": "gpt-5.5",
      "instructions": "Fallback prompt: used only while the node cannot read its agent ILK.",
      "model_settings": { "max_output_tokens": 1024 }
    }
  }
}
EOF
```

- **The minimum for an `ai_chat` node** is `behavior.kind` and `behavior.model`. The model must
  belong to the hive's provider.
- **The config fields:**
  - `behavior.kind`: `ai_chat`, or `echo` (no model call; for tests);
  - `behavior.model`;
  - `behavior.instructions`: a string, or `{"source": "inline", "value": "..."}`;
  - `behavior.model_settings`: `max_output_tokens`;
  - `behavior.base_url`;
  - `behavior.capabilities.multimodal`: `true` by default.

  Each field is described in `nodes/ai/ai-generic/README.md`.
- **Leave `temperature` out:** current models reject it (gpt-5.5, Claude 5; FINDINGS A-69).
- **`behavior.instructions` is a fallback.** Once the node reads its agent ILK, the ILK's definition
  is the prompt (step 5).
- **There is one config:** `config.json` in the node's directory
  (`/var/lib/fluxbee/nodes/AI/<node@hive>/config.json`).
  - `CONFIG_SET` validates it, applies it without a restart, and writes it.
  - `PUT .../nodes/<node>/config` only writes it; the node reads it at its next start.
  - A `CONFIG_SET` with the current `config_version` changes nothing; a lower one is refused
    (`stale_config_version`).
- **`CONFIG_SET` refuses:**
  - secrets: `config.secrets.*`, `behavior.api_key`, `behavior.api_key_env`, `behavior.openai`;
  - `behavior.vault_key`;
  - cognitive assets (`config.assets`), which go through `set_ilk_definition`;
  - any unknown field under `behavior`.
- **Of `runtime`, the node reads `runtime.immediate_memory`** (off by default) **and
  `runtime.cognitive_definition`.** Timeouts, pool and queue sizes are not applied to a node the
  orchestrator launched: it runs with 60 s per message, 4 workers and a queue of 128.

---

## 5) Give it its role (`set_ilk_definition`)

1. **Archi (SY.architect) writes the assets.** Each one is a role, skill, handbook or personality,
   stored as `blob://agent-assets/<sha256>.json`; Archi returns its hash. Archi does it with its
   asset tools, or with `POST /api/agent-assets` on its own HTTP API, as
   `scripts/agent_cognitive_definition_e2e.sh` does. Schema:
   `docs/identity-v2.1-agent-definition-addendum.md`.
2. **Find the node's ILK** (step 3).
3. **Apply the hashes:**

   ```bash
   ILK="ilk:<uuid>"
   curl -sS -X POST "$BASE/hives/motherbee/identity/ilks/$ILK/definition" \
     -H 'content-type: application/json' \
     -d '{"definition":{"role_hash":"<64 hex>","skill_hashes":["<64 hex>"],"handbook_hashes":[],"personality_hash":null}}'
   ```

   Up to 16 skills and 8 handbooks. `null` or `[]` clears a part.
4. **The node picks it up without a restart,** within `runtime.cognitive_definition.poll_interval_secs`
   (10 s by default). `CONFIG_GET` then shows `definition_state: "composed"`, or `"partial"` if an
   asset failed to load.

What the agent says:

- **With no hashes, or when no asset loads,** the agent runs a fixed prompt and the model answers only
  `{"status": "unconfigured", ...}`.
- **Assets are read on the node's own hive** (`/var/lib/fluxbee/blob/agent-assets/`). Archi writes
  them on the motherbee, and Syncthing replicates `blob/active/` between hives but not
  `agent-assets/`. An agent on worker1 finds its assets only if they are on worker1.

---

## 6) Update `ai.generic` without a new `.deb` (development)

`scripts/deploy-ia-node.sh` publishes a build of the runner and sends the `SYSTEM_UPDATE`. It uses
`scripts/publish-ia-runtime.sh`, which builds `ai_node_runner` and publishes it with
`scripts/publish-runtime.sh` into the local `/var/lib/fluxbee/dist`. Run it on the motherbee.

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "ai.generic" \
  --version "0.1.99" \
  --sudo
```

1. **Publish:** the new version becomes `current`.
2. **Update:** `POST /hives/$HIVE_ID/update` with `category=runtime`, retried while it answers
   `sync_pending` (`--update-retries`, default 8).
   - `--update-scope targeted`, the default, sends `runtime` and `runtime_version`, so drift in other
     runtimes does not block this one.
   - `--update-scope global` is only for checking the hive's overall state.
   - `--sync-hint` posts a `dist` sync hint to the hive before each attempt.
3. **The update restarts no node.** A running node moves to the new version when SY.admin restarts it.
   The restart recreates the node's unit at the version now `current`, for a node launched with
   `runtime_version: current`; a node pinned to a version keeps it.

   ```bash
   curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE/restart" -H 'content-type: application/json' \
     -d '{"node_name":"'"$NODE"'"}'
   ```

Other modes of the script:

- **`--node-name` with `--update-existing`:** after the update, it restarts the node's unit with the
  local `systemctl`.
  - That relaunches the binary the unit was created with, not the new version, so restart the node
    through SY.admin as above.
  - It only finds units of the host it runs on. A node on worker1 is not restarted, and the script
    fails.
- **`--node-name` alone:** after the update it creates the node (`POST .../nodes`), with the config in
  `--config-json <file>` and the tenant in `--tenant-id`.
  - Without `--config-json` it spawns with `config={}`, and the node stays UNCONFIGURED.
  - An existing node answers `NODE_ALREADY_EXISTS`. Removing it first takes
    `DELETE .../nodes/<node>` with the body `{"purge_instance": true}`, the full teardown: its ILK goes
    too, so the new node gets a new ILK and needs its definition again (step 5).
- **Immediate memory:** these flags write `runtime.immediate_memory` into the spawn config. Without
  them, the config is exactly the one in `--config-json`.
  - `--immediate-memory-enabled <true|false>`
  - `--immediate-memory-recent-max <n>`
  - `--immediate-memory-active-max <n>`
  - `--immediate-memory-summary-max-chars <n>`
  - `--immediate-memory-refresh-every-turns <n>`
  - `--immediate-memory-trim-noise <true|false>`

---

## 7) Attachments

**In:** `AI.*` reads `text/v1` `content_ref` and `attachments[]` through the AI SDK, up to 8
attachments of 10 MB each.

| Attachment | Reaches the model as |
| --- | --- |
| text (`text/plain`, `text/markdown`, `application/json`) | text |
| an image (`png`, `jpeg`, `webp`, `gif`) | an image: OpenAI `input_image` with a data URL, Anthropic an `image` block |
| any other file, with `multimodal` on | a document: OpenAI `input_file` (`file_data` as a data URL, plus `filename`); Anthropic takes PDF only |

With `multimodal` off, only text attachments are accepted.

**Out:**

- **The reply carries `attachments[]`,** and IO.slack uploads them to Slack.
- **The runner's tools make the files:** csv, text, json, markdown, html, pdf, xlsx, docx, png, jpeg.
- **`publish_html_page`** publishes an HTML page with a public URL, through SY.admin. It exists only
  when the node has its ILK.
