# Fluxbee — Node Package Install Specification (AI / IO / config_only)

**Status:** v1.0 draft
**Date:** 2026-04-15
**Audience:** Platform developers, Archi designers, operators
**Related specs:** `runtime-packaging-cli-spec.md` (existing package format), `sy-wf-rules-spec.md` (WF nodes use a different path), `01-arquitectura.md`

---

## 1. Scope

This spec covers the installation of **non-WF node packages**: `full_runtime`, `config_only`, and any future package types that carry binaries, scripts, or assets (AI agents, IO nodes, custom integrations).

**WF nodes are NOT in scope.** Workflow definitions are managed by `SY.wf-rules` (see `sy-wf-rules-spec.md`), which follows the same code-submission model as `SY.opa-rules`. WF nodes never go through the package install pipeline described here.

The existing package format (defined in `runtime-packaging-cli-spec.md`) is unchanged. This spec adds:

1. A standard HTTP endpoint in `SY.admin` for installing packages from blob storage.
2. The complete lifecycle from "package exists as a zip" to "package is installed in dist and ready to spawn".
3. Clear separation of responsibilities between admin (install), orchestrator (spawn/sync), and the operator.

---

## 2. Design principles

**HTTP-only.** No CLI tools are introduced. All operations go through `SY.admin` HTTP endpoints. Archi uses SCMD as the CLI client; operators use curl or any HTTP client.

**Blob as transport.** The zip file arrives via the existing blob storage (Syncthing-mediated). The operator (or Archi) places the zip in a known location in the blob, then calls an admin endpoint that reads it from there. No direct HTTP upload of binary payloads.

**Install is atomic.** A single admin endpoint receives a blob path, validates the package, installs it to dist, updates the manifest, and cleans up. The zip disappears after install — it is a transport artifact, not preserved.

**Spawn is separate.** Installing a package does NOT start a node. The operator spawns nodes explicitly via the existing `POST /hives/{hive}/nodes` endpoint. This keeps install and deploy as distinct, auditable operations.

**No versioned zip registry.** The dist directory (`/var/lib/fluxbee/dist/runtimes/<name>/<version>/`) IS the registry. The zip is just how packages get there. Once installed, the zip is deleted.

**Validation reuses fluxbee-publish logic.** The same validation code that `fluxbee-publish` uses today is called server-side by the admin endpoint. One validation logic, used everywhere.

---

## 3. Package types in scope

| Type | Description | Example |
|---|---|---|
| `full_runtime` | Complete node with binary/script, config, assets | AI agent, IO webhook receiver |
| `config_only` | Configuration on top of an existing runtime base | Custom config for an existing AI base |

These are the same types defined in `runtime-packaging-cli-spec.md`. The `workflow` package type exists in that spec but is superseded by `SY.wf-rules` for all practical purposes — workflow authors should use `SY.wf-rules` instead of packaging workflows as zips.

---

## 4. The install flow

### 4.1 End-to-end sequence

```
1. Author creates package directory following runtime-packaging-cli-spec.md format
2. Author zips the directory: zip -r my-ai-agent-0.1.0.zip my-ai-agent/
3. Author places zip in blob: blob://packages/incoming/my-ai-agent-0.1.0.zip
   (via Archi SCMD, cp to Syncthing dir, or any other mechanism)
4. Author calls: POST /admin/packages/install {"blob_path": "packages/incoming/my-ai-agent-0.1.0.zip"}
5. SY.admin:
   a. Reads zip from blob
   b. Extracts to staging
   c. Validates (same checks as fluxbee-publish)
   d. Copies to dist: /var/lib/fluxbee/dist/runtimes/my-ai-agent/0.1.0/
   e. Updates manifest.json
   f. Optionally sets as current version
   g. Optionally triggers sync to target hives
   h. Deletes staging dir and zip from blob
   i. Returns result
6. Operator spawns node when ready: POST /hives/{hive}/nodes {...}
```

### 4.2 Blob path convention

The blob path is relative to the **blob root**. The blob root is configured in `hive.yaml` via the `blob_path` field, with default value `/var/lib/fluxbee/blob`. Admin resolves the blob root from the hive configuration at startup.

Example: if `blob_path: /var/lib/fluxbee/blob` (the default) and the operator uploads a package at `/var/lib/fluxbee/blob/packages/incoming/foo-0.1.0.zip`, they pass `"blob_path": "packages/incoming/foo-0.1.0.zip"` in the API request. Admin resolves this to the absolute path by prepending the configured blob root.

Admin reads blob content via standard filesystem access — the blob is a local directory that Syncthing keeps in sync across hives. There is no separate blob API; admin uses `fs.read` on the resolved path.

Path convention for packages:

```
packages/incoming/<package-name>-<version>.zip
```

The admin endpoint does not enforce this convention — it accepts any valid blob path. But the convention is documented so operators and Archi can find packages predictably.

After successful install, the zip is deleted from the blob. After failed install, the zip is preserved in blob for debugging (the operator can inspect it and retry).

---

## 5. Admin endpoint: `POST /admin/packages/install`

### 5.1 Request

```json
{
  "blob_path": "packages/incoming/my-ai-agent-0.1.0.zip",
  "set_current": true,
  "sync_to": ["motherbee"],
  "dry_run": false
}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `blob_path` | string | Yes | Path to the zip in blob storage, relative to blob root |
| `set_current` | bool | No (default: false) | If true, set this version as `current` in manifest after install |
| `sync_to` | string[] | No | List of hive IDs to sync the package to after install |
| `dry_run` | bool | No (default: false) | If true, validate only — do not install. Zip is not deleted. |

### 5.2 Processing

**Step 1 — Read from blob.**

Admin reads the file at `<blob_root>/<blob_path>`. If the file does not exist, returns 404 with `BLOB_NOT_FOUND`.

If the file is larger than the configured limit (default: 100MB), returns 413 with `PACKAGE_TOO_LARGE`.

**Step 2 — Extract to staging.**

Create staging directory: `/var/lib/fluxbee/admin/staging/<request-id>/`. Extract zip contents there. If extraction fails (corrupt zip, unsupported format), return 400 with `INVALID_ZIP`.

The zip must contain a single root directory with a `package.json` at its root. If the structure doesn't match, return 422 with `INVALID_PACKAGE_LAYOUT`.

**Step 3 — Validate.**

Run the same validation that `fluxbee-publish` performs:

- `package.json` exists and parses.
- Required fields present: `name`, `version`, `type`.
- Name matches regex `^[a-z][a-z0-9.-]+$`.
- Version is valid semver.
- Type is one of `full_runtime`, `config_only`.
- For `full_runtime`: entry_point exists and is executable.
- For `config_only`: `runtime_base` is present.
- If `config_template` is specified, the file exists.

If validation fails, return 422 with `VALIDATION_FAILED` and the list of errors.

If `dry_run: true`, stop here and return the validation result without installing.

**Step 4 — Check for conflicts.**

Read the dist manifest. If a package with the same `name` + `version` already exists:

- Compute hash of the new package contents.
- If hash matches existing: return 200 with `"already_installed": true`. Idempotent.
- If hash differs: return 409 with `VERSION_ALREADY_EXISTS`. Author must bump version.

**Step 5 — Install.**

Copy the extracted package to `/var/lib/fluxbee/dist/runtimes/<name>/<version>/`. Update `manifest.json` with the new entry.

If `set_current: true`, update the manifest to point this version as `current`.

**Step 6 — Sync (optional).**

For each hive in `sync_to`:

- Send `sync-hint` to `SY.orchestrator@<hive>` via L2 unicast.
- Log the sync request. Do NOT block waiting for sync completion (fire-and-forget).

**Step 7 — Cleanup.**

Delete the staging directory. Delete the zip from blob (unless validation failed, in which case preserve for debugging).

**Step 8 — Respond.**

### 5.3 Response

**Success (200):**
```json
{
  "ok": true,
  "package": {
    "name": "my-ai-agent",
    "version": "0.1.0",
    "type": "full_runtime",
    "installed_at": "2026-04-15T15:00:00Z",
    "dist_path": "/var/lib/fluxbee/dist/runtimes/my-ai-agent/0.1.0/",
    "set_current": true,
    "already_installed": false
  },
  "validation": {
    "checks_run": 8,
    "warnings": 0
  },
  "sync": {
    "requested": ["motherbee"],
    "status": "sync_hint_sent"
  },
  "request_id": "req-abc123"
}
```

**Validation error (422):**
```json
{
  "ok": false,
  "error": "VALIDATION_FAILED",
  "validation": {
    "errors": [
      {
        "check": "entry_point_exists",
        "message": "Entry point bin/start.sh not found in package"
      }
    ]
  },
  "request_id": "req-abc123"
}
```

**Conflict (409):**
```json
{
  "ok": false,
  "error": "VERSION_ALREADY_EXISTS",
  "details": {
    "name": "my-ai-agent",
    "version": "0.1.0",
    "existing_hash": "sha256:abc...",
    "uploaded_hash": "sha256:def...",
    "advice": "Bump version in package.json and re-pack"
  },
  "request_id": "req-abc123"
}
```

**Other errors:**

| Code | HTTP Status | Description |
|---|---|---|
| `BLOB_NOT_FOUND` | 404 | Zip file not found at blob_path |
| `PACKAGE_TOO_LARGE` | 413 | Zip exceeds size limit |
| `INVALID_ZIP` | 400 | Corrupt or unsupported zip format |
| `INVALID_PACKAGE_LAYOUT` | 422 | Missing package.json or wrong directory structure |
| `VALIDATION_FAILED` | 422 | Package validation errors |
| `VERSION_ALREADY_EXISTS` | 409 | Same name+version with different hash |
| `INSTALL_ERROR` | 500 | Filesystem error during install |

### 5.4 Audit

Every install attempt (success or failure) is logged:

- request_id
- caller identity (from admin auth context)
- blob_path
- package name + version + hash
- validation result summary
- install result
- sync targets if any
- timestamp

---

## 6. Complementary endpoints

### 6.1 `GET /admin/packages`

List all installed packages.

Response:
```json
{
  "ok": true,
  "packages": [
    {
      "name": "my-ai-agent",
      "versions": ["0.1.0", "0.2.0"],
      "current": "0.2.0",
      "type": "full_runtime"
    },
    {
      "name": "custom-io-config",
      "versions": ["1.0.0"],
      "current": "1.0.0",
      "type": "config_only"
    }
  ]
}
```

### 6.2 `GET /admin/packages/{name}`

Get details of a specific package.

Response:
```json
{
  "ok": true,
  "name": "my-ai-agent",
  "type": "full_runtime",
  "current": "0.2.0",
  "versions": [
    {
      "version": "0.1.0",
      "installed_at": "2026-04-10T10:00:00Z",
      "hash": "sha256:abc..."
    },
    {
      "version": "0.2.0",
      "installed_at": "2026-04-15T15:00:00Z",
      "hash": "sha256:def..."
    }
  ]
}
```

### 6.3 `POST /admin/packages/{name}/set-current`

Set a specific version as current.

Request:
```json
{
  "version": "0.1.0"
}
```

Response:
```json
{
  "ok": true,
  "name": "my-ai-agent",
  "current": "0.1.0"
}
```

### 6.4 `DELETE /admin/packages/{name}/{version}`

Remove a specific version from dist. Refuses if it's the current version (must set-current to another version first) or if any running node uses this version.

---

## 7. Complete lifecycle: from author to running node

### 7.1 New AI agent

```bash
# 1. Author creates package directory (manually or with tooling of their choice)
mkdir my-classifier
# ... create package.json, bin/start.sh, config/, etc.

# 2. Zip it
cd .. && zip -r my-classifier-0.1.0.zip my-classifier/

# 3. Place in blob
cp my-classifier-0.1.0.zip /var/lib/fluxbee/blob/packages/incoming/

# 4. Install via admin
curl -X POST http://localhost:8080/admin/packages/install \
  -H "Content-Type: application/json" \
  -d '{
    "blob_path": "packages/incoming/my-classifier-0.1.0.zip",
    "set_current": true,
    "sync_to": ["motherbee"]
  }'

# 5. Verify installation
curl http://localhost:8080/admin/packages/my-classifier

# 6. Spawn node (separate operation, existing endpoint)
curl -X POST http://localhost:8080/hives/motherbee/nodes \
  -H "Content-Type: application/json" \
  -d '{
    "node_name": "AI.classifier",
    "runtime": "my-classifier",
    "runtime_version": "current",
    "tenant_id": "tnt:xxx"
  }'

# 7. Verify node is running
curl http://localhost:8080/hives/motherbee/nodes/AI.classifier@motherbee/status
```

### 7.2 Update existing package

```bash
# 1. Edit package, bump version to 0.2.0 in package.json
# 2. Re-zip
zip -r my-classifier-0.2.0.zip my-classifier/
cp my-classifier-0.2.0.zip /var/lib/fluxbee/blob/packages/incoming/

# 3. Install new version
curl -X POST http://localhost:8080/admin/packages/install \
  -d '{"blob_path":"packages/incoming/my-classifier-0.2.0.zip","set_current":true}'

# 4. Restart node to pick up new version (or kill + respawn)
curl -X DELETE http://localhost:8080/hives/motherbee/nodes/AI.classifier@motherbee
curl -X POST http://localhost:8080/hives/motherbee/nodes \
  -d '{"node_name":"AI.classifier","runtime":"my-classifier","runtime_version":"current","tenant_id":"tnt:xxx"}'
```

### 7.3 Archi installs a package

```
1. Archi generates package files (package.json, bin/start.sh, config, etc.)
2. Archi zips them locally or via a helper
3. Archi places zip in blob via SCMD blob-write command
4. Archi calls POST /admin/packages/install via SCMD http command
5. Archi calls POST /hives/{hive}/nodes to spawn
```

Each step is a separate SCMD action. Archi chains them as needed, with error handling at each step.

---

## 8. What fluxbee-publish becomes

Update 2026-04-21:

- the canonical publish path is now `SY.admin` `publish_runtime_package`
- Archi/executor plans use that same action
- `fluxbee-publish` remains only as a deprecated local compatibility wrapper over the shared core

The existing `fluxbee-publish` CLI can still work for operators with direct filesystem access, but it is no longer the recommended primary path:

```bash
fluxbee-publish ./my-classifier-0.1.0.zip --set-current
```

Internally, `fluxbee-publish` uses the same shared validation/install core as the admin endpoint, but runs it locally without going through the canonical system action.

The admin endpoint reuses the same validation and install logic as `fluxbee-publish`, extracted into a shared library. There is NO duplication of validation rules between the two paths.

`fluxbee-publish` is now deprecated as the primary operator path. It remains only as a migration/debugging tool while docs and workflows converge on admin/Archi publish.

---

## 9. What is NOT in scope

- WF workflow installation (handled by SY.wf-rules, not this pipeline)
- SY node extension packages (not supported in v1)
- Direct HTTP upload of zip payloads (blob is the transport layer)
- CLI tools beyond existing fluxbee-publish (all new operations are HTTP endpoints)
- Package registry, search, or discovery beyond listing installed packages
- Automatic node restart on package update (spawn/restart is always explicit)
- Multi-arch packaging
- Package signing or verification

---

## 10. Decisions

| Decision | Rationale |
|---|---|
| Blob as transport, not HTTP upload | Reuses existing infrastructure; Archi already has blob access; avoids HTTP streaming complexity |
| Admin does install, orchestrator does spawn | Clean separation of "installing software" vs "operating nodes" |
| Zip deleted after install | Zip is transport, not storage; dist is the registry |
| No CLI tools added | HTTP-only; Archi SCMD is the CLI for humans |
| Validation shared with fluxbee-publish | Single source of truth for what's valid |
| Spawn is separate from install | Explicit operator control; auditable |
| WF excluded from this pipeline | SY.wf-rules handles WF as code, not as packages |
| Dry-run preserves zip on failure | Debugging aid; operator can inspect and fix |

---

## 11. References

| Topic | Document |
|---|---|
| Package format and validation rules | `runtime-packaging-cli-spec.md` |
| WF workflow management (separate path) | `sy-wf-rules-spec.md` |
| WF node spec | `wf-v1.md` |
| Architecture | `01-arquitectura.md` |
| Existing fluxbee-publish | `rust/fluxbee-publish/` |
| Admin endpoints | `01-arquitectura.md`, admin API specs |
| Orchestrator node lifecycle | `sy-orchestrator-v2.md` |
