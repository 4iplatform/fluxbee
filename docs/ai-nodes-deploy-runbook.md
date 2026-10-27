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

- **The provider is the node's** (agent-tools-v1 T8): `behavior.provider`, `openai` or `anthropic`
  (step 4). Without it, the node uses the hive's: `ai.default_provider` in
  `/etc/fluxbee/hive.yaml`, or `openai` when there is no `ai:` section, read on each request. When
  the section exists, it must name the provider's model (`ai.providers.<provider>.model`). One node
  may run on Claude and another on OpenAI; `CONFIG_GET` shows the one in effect in `ai_provider`.
- **The node reads its key on every request,** from SY.vault, by its provider's `resource_type`
  (`openai` or `anthropic`), only in its own tenant:
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
      "model_settings": { "max_output_tokens": 1024 }
    }
  }
}
EOF
```

- **The minimum for an `ai_chat` node** is `behavior.kind` and `behavior.model`. The model must
  belong to the node's provider.
- **The config fields:**
  - `behavior.kind`: `ai_chat`, or `echo` (no model call; for tests);
  - `behavior.model`;
  - `behavior.provider`: `openai` or `anthropic`; without it, the hive's (step 2);
  - `behavior.model_settings`: `max_output_tokens`. An answer cut at that limit is a
    `provider_output_truncated` error;
  - `behavior.base_url`: it needs `behavior.provider`, and it must be `https` on that provider's
    host (`api.openai.com` or `api.anthropic.com`);
  - `behavior.multimodal`: `true` by default. The old `behavior.capabilities.multimodal` is refused;
    a `config.json` that still has it is converted once when the node starts, and its journal says
    so in one INFO line, which names the copy of the file as it was, `config.json.pre-0.1.86`
    ([Rolling back to 0.1.85](#rolling-back-to-0185)).

  Each field is described in `nodes/ai/ai-generic/README.md`.
- **Leave `temperature` out:** current models reject it (gpt-5.5, Claude 5; FINDINGS A-69).
- **The prompt is not config.** It is the agent ILK's definition (step 5). Until the node has one, it
  runs a fixed "unconfigured" prompt.
- **There is one config:** `config.json` in the node's directory
  (`/var/lib/fluxbee/nodes/AI/<node@hive>/config.json`).
  - `CONFIG_SET` validates it, applies it without a restart, and writes it.
  - `PUT .../nodes/<node>/config` only writes it; the node reads it at its next start.
  - A `CONFIG_SET` with the current `config_version` changes nothing; a lower one is refused
    (`stale_config_version`).
- **`CONFIG_SET` refuses:**
  - secrets: `config.secrets.*`, `behavior.api_key`, `behavior.api_key_env`, `behavior.openai`;
  - `behavior.vault_key`;
  - `behavior.capabilities` (now `behavior.multimodal`);
  - a `behavior.base_url` without `behavior.provider`, or on the other provider's host;
  - cognitive assets (`config.assets`), which go through `set_ilk_definition`;
  - any unknown field under `behavior` or `runtime`, `behavior.instructions` included.
- **`runtime` has two parts:**
  - `runtime.immediate_memory` (`enabled`, off by default; `recent_interactions_max`;
    `summary_max_chars`) applies with the `CONFIG_SET`;
  - `runtime.cognitive_definition` (`poll_interval_secs`, `blob_root`) applies at the next start.
- **Timeouts, workers and queue are the AI runtime's, not config:**
  - each model call has 5 min, its provider's own retries included;
  - a message has 47 min: up to 8 model turns of a tool loop, one plain call after them, and 2 min
    for its tools;
  - 4 workers and a queue of 128.
- **A message runs once.** When it fails or runs out of time, the sender gets an `ai_runtime_error` or
  `ai_runtime_timeout` error and the message is not run again: it may already have called the model
  or a tool such as `publish_html_page`.

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

- **With no hashes, when no asset loads, or while the node has not found its agent ILK,** the agent
  runs a fixed prompt and the model answers only `{"status": "unconfigured", ...}`.
- **Once the agent has a definition, only a definite new one replaces it:** its ILK is there, is an
  agent, and every asset it names was read (an asset that is there but invalid, wrong hash or not a
  regular file, is left out). A poll that cannot read the identity SHM, does not find the ILK in it
  (a degraded identity replica) or misses an asset changes nothing: the agent keeps its definition,
  and `CONFIG_GET` shows why in `definition.last_poll_error`.
- **Assets are read on the node's own hive** (`/var/lib/fluxbee/blob/agent-assets/`). Archi writes
  them on the motherbee, and the Syncthing folder `fluxbee-dist-agent-assets` (send-only on the
  motherbee, receive-only on every worker) brings them to every worker, about 10 to 20 s after they
  are written, while the ILK's new hashes arrive in about 1 s. Meanwhile an agent on a worker keeps
  its previous definition and takes the new one at the next poll after the assets arrive (10 s by
  default); one that starts before its assets arrive answers with the "unconfigured" prompt until
  then.

---

## 6) Update `ai.generic` without a new `.deb` (development)

The standard path: publish the runtime on the motherbee, update the node's hive, and restart the
node through SY.admin.

1. **Build and publish**, on the motherbee, from the repo:

   ```bash
   bash scripts/publish-ia-runtime.sh --runtime ai.generic --version 0.1.99 --set-current --sudo
   ```

   - It builds `ai_node_runner` (`--skip-build` publishes `target/release/ai_node_runner` as it is)
     into `/var/lib/fluxbee/dist/runtimes/ai.generic/0.1.99/`.
   - `--set-current` makes it the version `current` points at.
   - It prints `manifest_version=` and `manifest_hash=`; the update takes both.
2. **Update the node's hive** (`POST /hives/{hive}/update`):

   ```bash
   curl -sS -X POST "$BASE/hives/$HIVE_ID/update" -H 'content-type: application/json' \
     -d '{"category":"runtime","manifest_version":<n>,"manifest_hash":"<hash>","runtime":"ai.generic","runtime_version":"0.1.99"}'
   ```

   - `runtime` and `runtime_version` limit the check to this runtime, so drift in another one does
     not block it.
   - `sync_pending` (HTTP 202) means the hive does not have the new manifest or files yet: Syncthing
     has not delivered them. Send the same request again.
   - On a worker, a scan on the motherbee announces them at once (a scan on the worker pulls
     nothing):

     ```bash
     curl -sS -X POST "$BASE/hives/motherbee/sync-hint" -H 'content-type: application/json' \
       -d '{"channel":"dist","folder_id":"fluxbee-dist-runtimes"}'
     ```

3. **Restart the node through SY.admin** (`POST /hives/{hive}/nodes/{node}/restart`). The update
   restarts no node.

   ```bash
   curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE/restart"
   ```

   - The restart recreates the node's unit at the version `current` now points at, for a node
     launched with `runtime_version: current`; a node pinned to a version keeps it. The answer's
     `version` is the one it runs.
   - It works on any hive: SY.admin sends it to the motherbee's orchestrator, which passes it to
     the node's hive.
   - Do not restart the unit with a local `systemctl`: that relaunches the binary the unit was
     created with.

### Rolling back to 0.1.85

0.1.86 renamed `behavior.capabilities.multimodal` to `behavior.multimodal` and added
`behavior.provider`, and 0.1.85 refuses both: a node that goes back to 0.1.85 with a 0.1.86
`config.json` boots FAILED_CONFIG. Before its restart at 0.1.85, on the node's hive, in its
directory:

- **If 0.1.86 converted its `config.json`** when it first started, the file as it was is next to
  it, `config.json.pre-0.1.86` (mode 0600); the conversion's INFO line names it. Restore it:

  ```bash
  cd "/var/lib/fluxbee/nodes/AI/$NODE"
  sudo cp -p config.json.pre-0.1.86 config.json
  ```

  A `CONFIG_SET` applied under 0.1.86 is not in it: apply it again after the restart.
- **Otherwise, edit `config.json`:** turn `behavior.multimodal` back into
  `behavior.capabilities.multimodal`, and drop `behavior.provider`. The node then runs on the
  hive's provider, so `behavior.model` and a `behavior.base_url` must be that provider's.

Then restart the node through SY.admin (step 3 above), at 0.1.85.

**A new node** with the new version is a `run_node` (step 3).

**Removing a node** is a `kill_node` with `purge_instance`, the full teardown:

```bash
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE" -H 'content-type: application/json' \
  -d '{"purge_instance":true}'
```

- Its instance directory and its `config.json` go.
- Its ILK goes too, so a new node with the same name gets a new ILK and needs its definition again
  (step 5).
- Without `purge_instance` the node only stops, and a `run_node` with its name answers
  `NODE_ALREADY_EXISTS`.

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
  One file goes out per reply, with the text of the call that made it.
- **`publish_html_page`** publishes an HTML page with a public URL, through SY.admin. It exists only
  when the node has its ILK, and the link expires after 24 h.
- **Every tool, with its limits:** the tool catalog,
  [reference/agent-tools.md](reference/agent-tools.md), generated from the code. A running node
  answers the same cards to `NODE_HELP`.
