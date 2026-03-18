# Runtime Packaging and Install CLI

**Status:** Draft v1.1
**Date:** 2026-03-16
**Audience:** Developers building node runtimes, orchestrator, and operational tooling
**Parent specs:** `runtime-lifecycle-spec.md`, `software-distribution-spec.md`, `node-spawn-config-spec.md`

---

## 1. Problem

Publishing a new runtime today requires manual SSH access to motherbee, creating directories, writing files, and running a Python script to update the manifest. This is error-prone, not automatable by an AI architect, and doesn't support the three package types we need for the target operating model.

This document defines:
1. A standard package format for all node types.
2. A CLI tool (`fluxbee-publish`) that validates and installs packages.
3. Changes to orchestrator to support all package types at spawn time.

---

## 2. The Three Package Types

| Type | What's in the package | Runtime at spawn | Example |
|------|----------------------|------------------|---------|
| `full_runtime` | `bin/start.sh` + binaries + optional config template | Uses its own `bin/start.sh` | A custom AI node with specialized binary |
| `config_only` | Config template + assets (prompts, data files) | Uses an existing base runtime's `bin/start.sh` | A support agent using the generic `ai.generic` runtime with a specific prompt |
| `workflow` | Flow definition + optional scripts | Uses the WF engine runtime's `bin/start.sh` | An onboarding workflow running on `wf.engine` |

All three types use the same package format and the same CLI to publish.

---

## 3. Package Format

### 3.1 Directory Structure (Source)

The developer works in a normal project directory. The structure follows what Rust (cargo) and other tools already produce, with one addition: `package.json` at the root.

**Full runtime (custom binary):**

```
my-ai-frontdesk/
├── package.json              # Required: package metadata
├── bin/
│   └── start.sh              # Required for full_runtime
├── lib/                      # Optional: shared libraries
│   └── libmodel.so
├── assets/                   # Optional: data files, models
│   └── prompts/
│       └── default.txt
└── config/                   # Optional: default config template
    └── default-config.json
```

**Config-only (uses base runtime):**

```
my-soporte-config/
├── package.json              # Required: declares runtime_base
├── assets/
│   └── prompts/
│       ├── system.txt
│       └── escalation.txt
└── config/
    └── default-config.json   # Template that orchestrator uses at spawn
```

**Workflow (uses WF engine):**

```
my-onboarding-flow/
├── package.json              # Required: declares runtime_base
├── flow/
│   └── definition.json       # Workflow definition
├── scripts/                  # Optional: auxiliary scripts
│   └── validate-input.sh
└── config/
    └── default-config.json
```

### 3.2 package.json (Required)

```json
{
  "name": "ai.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime",
  "description": "Government frontdesk AI with specialized document processing",
  "runtime_base": null,
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
```

**Fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Runtime name. Must match the naming convention: lowercase, dots as separators (e.g., `ai.soporte`, `wf.onboarding`, `io.whatsapp.gov`) |
| `version` | Yes | Semver version string |
| `type` | Yes | `full_runtime`, `config_only`, or `workflow` |
| `description` | No | Human-readable description |
| `runtime_base` | Required for `config_only` and `workflow` | Name of the base runtime to use (must already be published). Null for `full_runtime` |
| `config_template` | No | Path to default config template within the package. Orchestrator can use this as base for config.json at spawn if no explicit config is provided |
| `entry_point` | No | Path to entry point within the package. Defaults to `bin/start.sh` for `full_runtime`. Ignored for `config_only` and `workflow` (uses base runtime's entry point) |

### 3.3 Examples

**Full runtime:**
```json
{
  "name": "ai.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime",
  "description": "Government frontdesk AI",
  "runtime_base": null,
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
```

**Config-only:**
```json
{
  "name": "ai.soporte.billing",
  "version": "2.1.0",
  "type": "config_only",
  "description": "Billing support agent config for ai.generic",
  "runtime_base": "ai.generic",
  "config_template": "config/default-config.json"
}
```

**Workflow:**
```json
{
  "name": "wf.onboarding.standard",
  "version": "1.0.0",
  "type": "workflow",
  "description": "Standard customer onboarding flow",
  "runtime_base": "wf.engine",
  "config_template": "config/default-config.json"
}
```

---

## 4. CLI Tool: `fluxbee-publish`

### 4.1 Purpose

Validates a package directory and publishes it to the local dist on motherbee. Runs on motherbee only (needs filesystem access to `/var/lib/fluxbee/dist/`).

### 4.2 Usage

```bash
# Publish from a project directory
fluxbee-publish /path/to/my-ai-frontdesk

# Publish from current directory
cd my-ai-frontdesk && fluxbee-publish .

# Publish a specific version (overrides package.json version)
fluxbee-publish /path/to/project --version 1.1.0

# Dry run (validate only, don't install)
fluxbee-publish /path/to/project --dry-run

# Publish and trigger sync+update to a worker
fluxbee-publish /path/to/project --deploy worker-220
```

### 4.3 What the CLI Does

```
1. READ package.json from source directory
   - Validate required fields (name, version, type)
   - Validate name format (lowercase, dots)
   - Validate version format (semver)

2. VALIDATE package structure based on type:
   - full_runtime: bin/start.sh must exist and be executable
   - config_only: runtime_base must be specified
   - workflow: runtime_base must be specified, flow/ directory should exist

3. VALIDATE runtime_base exists (if config_only or workflow):
   - Check /var/lib/fluxbee/dist/runtimes/<runtime_base>/ exists
   - Check at least one version of base runtime is published

4. COPY package to dist:
   - Target: /var/lib/fluxbee/dist/runtimes/<name>/<version>/
   - Copy entire directory structure
   - Set permissions: directories 0755, files 0644, bin/* 0755

5. UPDATE manifest:
   - Read /var/lib/fluxbee/dist/runtimes/manifest.json
   - Add version to runtimes.<name>.available
   - Set runtimes.<name>.current to this version
   - Add runtimes.<name>.type and runtimes.<name>.runtime_base
   - Bump manifest version (epoch ms)
   - Atomic write (tmp + rename)

6. VERIFY readiness:
   - For full_runtime: confirm bin/start.sh exists and is executable in dist
   - For config_only/workflow: confirm base runtime is ready

7. REPORT:
   - Print summary: name, version, type, dist path, manifest version
   - If --deploy: trigger sync-hint + update to target hive

8. OPTIONAL DEPLOY (--deploy <hive>):
   - POST /hives/<hive>/sync-hint (wait for dist convergence)
   - POST /hives/<hive>/update (with new manifest version/hash)
   - Report readiness status on target hive
```

### 4.4 Output

```
$ fluxbee-publish ./my-soporte-config

fluxbee-publish v1.0
────────────────────
Package:     ai.soporte.billing
Version:     2.1.0
Type:        config_only
Base:        ai.generic
Template:    config/default-config.json

Validating... OK
  ✓ package.json valid
  ✓ runtime_base ai.generic exists (current: 1.0.0)
  ✓ config template found

Installing to /var/lib/fluxbee/dist/runtimes/ai.soporte.billing/2.1.0/
  ✓ Files copied (3 files, 12KB)
  ✓ Permissions set

Updating manifest...
  ✓ Manifest updated (version: 1710432000000)

Ready to deploy.
  Run: fluxbee-publish . --deploy worker-220
  Or:  curl -X POST http://127.0.0.1:8080/hives/worker-220/update ...
```

### 4.5 Error Cases

```
$ fluxbee-publish ./broken-package

fluxbee-publish v1.0
────────────────────
Package:     ai.broken
Version:     1.0.0
Type:        full_runtime

Validating... FAILED
  ✗ bin/start.sh not found (required for full_runtime)

Publish aborted. Fix the errors and retry.
```

```
$ fluxbee-publish ./config-no-base

fluxbee-publish v1.0
────────────────────
Package:     ai.custom
Version:     1.0.0
Type:        config_only
Base:        ai.nonexistent

Validating... FAILED
  ✗ runtime_base "ai.nonexistent" not found in dist

Publish aborted. Publish the base runtime first.
```

---

## 5. Dist Layout After Publish

### 5.1 Full Runtime

```
/var/lib/fluxbee/dist/runtimes/
├── manifest.json
└── ai.frontdesk.gov/
    └── 1.0.0/
        ├── package.json
        ├── bin/
        │   └── start.sh
        ├── lib/
        │   └── libmodel.so
        ├── assets/
        │   └── prompts/
        │       └── default.txt
        └── config/
            └── default-config.json
```

### 5.2 Config-Only

```
/var/lib/fluxbee/dist/runtimes/
├── manifest.json
├── ai.generic/                    # Base runtime (already published)
│   └── 1.0.0/
│       ├── package.json
│       └── bin/
│           └── start.sh
└── ai.soporte.billing/            # Config-only package
    └── 2.1.0/
        ├── package.json
        ├── assets/
        │   └── prompts/
        │       ├── system.txt
        │       └── escalation.txt
        └── config/
            └── default-config.json
```

### 5.3 Workflow

```
/var/lib/fluxbee/dist/runtimes/
├── manifest.json
├── wf.engine/                     # Base runtime (already published)
│   └── 1.0.0/
│       ├── package.json
│       └── bin/
│           └── start.sh
└── wf.onboarding.standard/        # Workflow package
    └── 1.0.0/
        ├── package.json
        ├── flow/
        │   └── definition.json
        ├── scripts/
        │   └── validate-input.sh
        └── config/
            └── default-config.json
```

---

## 6. Manifest Extension

The manifest gains two new optional fields per runtime to support the three package types:

```json
{
  "schema_version": 2,
  "version": 1710432000000,
  "updated_at": "2026-03-14T10:00:00Z",
  "runtimes": {
    "ai.generic": {
      "available": ["1.0.0"],
      "current": "1.0.0",
      "type": "full_runtime",
      "runtime_base": null
    },
    "ai.soporte.billing": {
      "available": ["2.0.0", "2.1.0"],
      "current": "2.1.0",
      "type": "config_only",
      "runtime_base": "ai.generic"
    },
    "wf.engine": {
      "available": ["1.0.0"],
      "current": "1.0.0",
      "type": "full_runtime",
      "runtime_base": null
    },
    "wf.onboarding.standard": {
      "available": ["1.0.0"],
      "current": "1.0.0",
      "type": "workflow",
      "runtime_base": "wf.engine"
    }
  }
}
```

`schema_version` bumps to 2. Runtimes without `type` field are treated as `full_runtime` for backward compatibility.

---

## 7. Orchestrator Changes

### 7.1 Spawn Resolution by Package Type

When orchestrator receives a spawn request, it resolves the entry point based on the package type:

```rust
fn resolve_entry_point(runtime: &str, version: &str, manifest: &Manifest) -> Result<PathBuf> {
    let runtime_info = manifest.runtimes.get(runtime)
        .ok_or(SpawnError::RuntimeNotAvailable)?;

    let resolved_version = if version == "current" {
        runtime_info.current.clone()
    } else {
        version.to_string()
    };

    let package_type = runtime_info.r#type.as_deref().unwrap_or("full_runtime");
    let dist_base = PathBuf::from("/var/lib/fluxbee/dist/runtimes");

    match package_type {
        "full_runtime" => {
            // Use own bin/start.sh
            let start_sh = dist_base
                .join(runtime)
                .join(&resolved_version)
                .join("bin/start.sh");
            if !start_sh.exists() || !is_executable(&start_sh) {
                return Err(SpawnError::RuntimeNotPresent { .. });
            }
            Ok(start_sh)
        }
        "config_only" | "workflow" => {
            // Use base runtime's bin/start.sh
            let base = runtime_info.runtime_base.as_ref()
                .ok_or(SpawnError::MissingRuntimeBase)?;
            let base_info = manifest.runtimes.get(base.as_str())
                .ok_or(SpawnError::BaseRuntimeNotAvailable)?;
            let base_version = base_info.current.clone();
            let start_sh = dist_base
                .join(base.as_str())
                .join(&base_version)
                .join("bin/start.sh");
            if !start_sh.exists() || !is_executable(&start_sh) {
                return Err(SpawnError::BaseRuntimeNotPresent { .. });
            }
            Ok(start_sh)
        }
        _ => Err(SpawnError::UnknownPackageType)
    }
}
```

### 7.2 Config Assembly at Spawn

When orchestrator creates config.json for a node, it layers configs:

```
1. Load default config template from package (if config/default-config.json exists):
   /var/lib/fluxbee/dist/runtimes/<runtime>/<version>/config/default-config.json

2. Merge with spawn request config (request overrides template):
   The params.config from POST /hives/{hive}/nodes

3. Inject _system block (always overrides):
   ilk_id, node_name, hive_id, runtime, runtime_version, created_at, created_by

4. For config_only/workflow, also inject:
   _system.runtime_base: base runtime name
   _system.package_path: path to the package in dist (for assets access)

5. Write final config.json
```

This means a `config_only` package can ship with sensible defaults in its template, and the spawn request only needs to override what's specific (e.g., API key, tenant_id).

### 7.3 Package Path in _system Block

For `config_only` and `workflow` packages, the node needs to know where its assets are (prompts, flow definitions, scripts). Orchestrator adds this to `_system`:

```json
{
  "_system": {
    "ilk_id": "ilk:...",
    "node_name": "AI.soporte.billing.l1@worker-220",
    "runtime": "ai.soporte.billing",
    "runtime_version": "2.1.0",
    "runtime_base": "ai.generic",
    "package_path": "/var/lib/fluxbee/dist/runtimes/ai.soporte.billing/2.1.0",
    ...
  }
}
```

The node reads assets from `_system.package_path`. For example, to load its prompt:

```rust
let package_path = config["_system"]["package_path"].as_str().unwrap();
let prompt = std::fs::read_to_string(
    PathBuf::from(package_path).join("assets/prompts/system.txt")
)?;
```

### 7.4 Readiness Check Extension

The readiness check in `GET /versions` extends for non-full runtimes:

```rust
fn check_readiness(runtime: &str, version: &str, manifest: &Manifest) -> Readiness {
    let runtime_info = &manifest.runtimes[runtime];
    let package_type = runtime_info.r#type.as_deref().unwrap_or("full_runtime");
    let dist_base = PathBuf::from("/var/lib/fluxbee/dist/runtimes");

    match package_type {
        "full_runtime" => {
            let start_sh = dist_base.join(runtime).join(version).join("bin/start.sh");
            Readiness {
                runtime_present: start_sh.exists(),
                start_sh_executable: start_sh.exists() && is_executable(&start_sh),
                base_runtime_ready: true, // not applicable
            }
        }
        "config_only" | "workflow" => {
            let package_dir = dist_base.join(runtime).join(version);
            let base = runtime_info.runtime_base.as_ref().unwrap();
            let base_info = &manifest.runtimes[base.as_str()];
            let base_start = dist_base
                .join(base.as_str())
                .join(&base_info.current)
                .join("bin/start.sh");
            Readiness {
                runtime_present: package_dir.exists(),
                start_sh_executable: base_start.exists() && is_executable(&base_start),
                base_runtime_ready: base_start.exists() && is_executable(&base_start),
            }
        }
        _ => Readiness::unknown()
    }
}
```

New field in readiness: `base_runtime_ready` (bool). For `full_runtime` always true. For others, indicates whether the base runtime's `start.sh` is available.

### 7.5 New Error Codes

| Code | Condition |
|------|-----------|
| `MISSING_RUNTIME_BASE` | Package is config_only/workflow but runtime_base not specified in manifest |
| `BASE_RUNTIME_NOT_AVAILABLE` | runtime_base not found in manifest |
| `BASE_RUNTIME_NOT_PRESENT` | runtime_base in manifest but start.sh missing locally |
| `UNKNOWN_PACKAGE_TYPE` | Package type not recognized |

---

## 8. Compatibility and Migration

### 8.1 Current Runtime Behavior (as of 2026-03-16)

Current orchestrator behavior is still FR-08 style:

- Runtime manifest requires `schema_version = 1`.
- Spawn preflight requires `dist/runtimes/<runtime>/<version>/bin/start.sh` to exist and be executable.
- `SYSTEM_UPDATE(category=runtime)` verifies `start.sh` for every runtime marked as `current`.
- `GET /versions` readiness exposes only:
  - `runtime_present`
  - `start_sh_executable`

This means `config_only` and `workflow` are NOT executable yet without orchestrator changes in this document.

### 8.2 Target Compatibility Contract

When PUB tasks are completed, compatibility rules become:

- Missing `type` in manifest entry -> treat as `full_runtime`.
- Missing `runtime_base` is valid only for `full_runtime`.
- Missing `package.json` in dist directory remains valid for legacy runtimes.
- Manifest loader accepts schema versions `1` and `2` during migration.
- Writer emits `schema_version: 2` once all target hives run compatibility code and compatibility gate `FLUXBEE_RUNTIME_MANIFEST_WRITE_V2=1` is enabled.

### 8.3 Migration Order (required)

1. Deploy orchestrator compatibility code (accept schema v1/v2 + package-type aware spawn/update/readiness).
2. Validate FR-08 regressions remain green with schema v1 manifests.
3. Enable `fluxbee-publish` to emit schema v2 manifest entries (`type`, `runtime_base`).
4. Roll out config-only/workflow packages.

---

## 9. Development Workflow: Source to Publish

### 9.1 Repository Structure

Node source code lives in the repo under `nodes/`. This is development code, not what orchestrator executes.

```
fluxbee/                            # Repo root (cargo workspace)
├── Cargo.toml                      # Workspace manifest
├── crates/
│   └── fluxbee-sdk/                # SDK used by all nodes
├── src/
│   └── bin/                        # System binaries (sy-*, rt-*)
└── nodes/                          # Node source code
    └── gov/
        ├── TEMPLATE_NODE.md        # Template for new nodes
        ├── common/                 # Shared code between gov nodes
        │   └── src/
        └── ai-frontdesk-gov/       # Specific node project
            ├── Cargo.toml
            ├── src/
            │   └── main.rs
            ├── prompts/            # Development prompts
            │   └── system.txt
            └── config/             # Development config templates
                └── default-config.json
```

**Key distinction:**

| Path | Purpose | Who uses it |
|------|---------|-------------|
| `nodes/<project>/` | Source code, development, compilation | Developer |
| `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/` | Installed package, ready for execution | Orchestrator |

The developer compiles in `nodes/`, packages the output, and publishes to `dist/`. Orchestrator never looks at `nodes/`.

### 9.2 Full Workflow: Custom Binary (full_runtime)

```bash
# 1. DEVELOP — work in source directory
cd fluxbee/nodes/gov/ai-frontdesk-gov
# edit src/main.rs, prompts/, config/...

# 2. COMPILE — build the binary
cargo build --release
# Output: fluxbee/target/release/ai-frontdesk-gov

# 3. PACKAGE — create the package directory
#    Can be done in a temp dir or in a package/ subdir of the project
mkdir -p package/bin package/assets/prompts package/config

# Copy binary as start.sh (orchestrator's entry point)
cp ../../../target/release/ai-frontdesk-gov package/bin/start.sh
chmod +x package/bin/start.sh

# Copy assets and config template from source
cp prompts/system.txt package/assets/prompts/
cp config/default-config.json package/config/

# Write package.json
cat > package/package.json <<EOF
{
  "name": "ai.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime",
  "description": "Government frontdesk AI with document processing",
  "config_template": "config/default-config.json"
}
EOF

# 4. PUBLISH — install to dist and update manifest
fluxbee-publish ./package

# 5. DEPLOY (optional) — sync to worker and verify
fluxbee-publish ./package --deploy worker-220

# 6. SPAWN — via admin API or ADMIN_COMMAND
curl -X POST "$BASE/hives/worker-220/nodes" \
  -H "Content-Type: application/json" \
  -d '{
    "node_name": "AI.frontdesk.gov",
    "runtime": "ai.frontdesk.gov",
    "runtime_version": "current",
    "config": {"api_key": "sk-..."}
  }'
```

### 9.3 Full Workflow: Config-Only (no compilation)

```bash
# 1. CREATE — just a directory with config and assets
mkdir -p my-billing-agent/assets/prompts my-billing-agent/config

cat > my-billing-agent/assets/prompts/system.txt <<EOF
You are a billing support specialist for Acme Corp.
You handle refunds, payment inquiries, and invoice disputes.
EOF

cat > my-billing-agent/config/default-config.json <<EOF
{
  "api_provider": "openai",
  "model": "gpt-4",
  "temperature": 0.7,
  "max_tokens": 4096
}
EOF

cat > my-billing-agent/package.json <<EOF
{
  "name": "ai.soporte.billing",
  "version": "2.1.0",
  "type": "config_only",
  "runtime_base": "ai.generic",
  "config_template": "config/default-config.json"
}
EOF

# 2. PUBLISH — no compile step, just publish
fluxbee-publish ./my-billing-agent --deploy worker-220

# 3. SPAWN
curl -X POST "$BASE/hives/worker-220/nodes" \
  -d '{
    "node_name": "AI.soporte.billing.l1",
    "runtime": "ai.soporte.billing",
    "runtime_version": "current",
    "config": {"api_key": "sk-...", "tenant_id": "tnt:..."}
  }'
```

### 9.4 Full Workflow: Workflow Definition

```bash
# 1. CREATE
mkdir -p my-onboarding/flow my-onboarding/config

cat > my-onboarding/flow/definition.json <<EOF
{
  "steps": ["greeting", "collect_data", "verify", "confirm"],
  "timeout_secs": 300,
  "on_timeout": "escalate"
}
EOF

cat > my-onboarding/config/default-config.json <<EOF
{
  "triggers": ["new_customer", "channel_switch"],
  "escalation_target": "AI.supervisor"
}
EOF

cat > my-onboarding/package.json <<EOF
{
  "name": "wf.onboarding.standard",
  "version": "1.0.0",
  "type": "workflow",
  "runtime_base": "wf.engine"
}
EOF

# 2. PUBLISH
fluxbee-publish ./my-onboarding --deploy worker-220

# 3. SPAWN
curl -X POST "$BASE/hives/worker-220/nodes" \
  -d '{
    "node_name": "WF.onboarding.standard",
    "runtime": "wf.onboarding.standard",
    "runtime_version": "current"
  }'
```

### 9.5 Automating the Package Step (Optional)

For Rust nodes that follow the repo convention (`nodes/<group>/<project>/`), the packaging step can be automated with a script per node:

```bash
#!/usr/bin/env bash
# nodes/gov/ai-frontdesk-gov/package.sh
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$PROJECT_DIR/../../.." && pwd)"
BINARY_NAME="ai-frontdesk-gov"
RUNTIME_NAME="ai.frontdesk.gov"
VERSION="${1:-0.0.1}"

# Build
(cd "$REPO_ROOT" && cargo build --release -p "$BINARY_NAME")

# Package
PKG="$PROJECT_DIR/package"
rm -rf "$PKG"
mkdir -p "$PKG/bin" "$PKG/assets/prompts" "$PKG/config"

cp "$REPO_ROOT/target/release/$BINARY_NAME" "$PKG/bin/start.sh"
chmod +x "$PKG/bin/start.sh"
[ -d "$PROJECT_DIR/prompts" ] && cp -r "$PROJECT_DIR/prompts/"* "$PKG/assets/prompts/"
[ -f "$PROJECT_DIR/config/default-config.json" ] && cp "$PROJECT_DIR/config/default-config.json" "$PKG/config/"

cat > "$PKG/package.json" <<EOF
{
  "name": "$RUNTIME_NAME",
  "version": "$VERSION",
  "type": "full_runtime",
  "config_template": "config/default-config.json"
}
EOF

echo "Package ready at $PKG"
echo "Run: fluxbee-publish $PKG"
```

Usage:

```bash
cd nodes/gov/ai-frontdesk-gov
bash package.sh 1.0.0
fluxbee-publish ./package --deploy worker-220
```

---

## 10. Quick Reference: Steps per Package Type

| Step | full_runtime | config_only | workflow |
|------|-------------|-------------|----------|
| 1. Develop | `nodes/<group>/<project>/` | Any directory | Any directory |
| 2. Compile | `cargo build --release` | Not needed | Not needed |
| 3. Package | Copy binary to `package/bin/start.sh` + assets + package.json | Create assets + config + package.json | Create flow/ + config + package.json |
| 4. Publish | `fluxbee-publish ./package` | `fluxbee-publish ./my-config` | `fluxbee-publish ./my-flow` |
| 5. Deploy | `--deploy worker-220` (or manual sync-hint + update) | Same | Same |
| 6. Spawn | `POST /hives/{hive}/nodes` | Same | Same |

**Requires base runtime?** full_runtime: No. config_only: Yes (`runtime_base` in package.json). workflow: Yes (`runtime_base` in package.json).

**Has bin/start.sh?** full_runtime: Yes (own binary). config_only: No (uses base). workflow: No (uses base).

---

## 11. Implementation Tasks

### Phase 0 — Baseline and Safety Gates (blocking)

- [x] PUB-T1. Add orchestrator tests that pin current FR-08 behavior before changes:
  - `schema_version=1` manifest load.
  - spawn preflight (`RUNTIME_NOT_PRESENT` when missing `start.sh`).
  - runtime readiness fields (`runtime_present`, `start_sh_executable`).
- [x] PUB-T2. Add migration-safe manifest loader contract:
  - accept runtime manifest `schema_version` in `[1,2]`.
  - reject unknown schema versions with explicit error.
- [x] PUB-T3. Keep FR-08 stable for legacy runtimes:
  - missing `type` => `full_runtime`.
  - `full_runtime` path stays `/bin/start.sh`.

### Phase 1 — Runtime Manifest v2 Model

- [x] PUB-T4. Introduce typed manifest runtime entry model:
  - `available`, `current`, optional `type`, optional `runtime_base`.
  - preserve unknown fields when possible (forward compatibility).
- [x] PUB-T5. Implement manifest read/write helpers used by orchestrator and publish CLI.
- [x] PUB-T6. Implement schema migration policy:
  - read v1/v2.
  - write v2 only after compatibility gate flag is enabled.

### Phase 2 — Orchestrator Spawn/Update/Readiness

- [x] PUB-T7. Implement package-type runtime entrypoint resolution in spawn:
  - `full_runtime`: own `bin/start.sh`.
  - `config_only`/`workflow`: base runtime `bin/start.sh`.
- [x] PUB-T8. Add spawn validation errors:
  - `MISSING_RUNTIME_BASE`
  - `BASE_RUNTIME_NOT_AVAILABLE`
  - `BASE_RUNTIME_NOT_PRESENT`
  - `UNKNOWN_PACKAGE_TYPE`
- [x] PUB-T9. Extend `SYSTEM_UPDATE(category=runtime)` verification to be package-type aware:
  - for `full_runtime`: validate own `start.sh`.
  - for `config_only`/`workflow`: validate package directory + base runtime `start.sh`.
- [x] PUB-T10. Extend readiness payload with `base_runtime_ready` while keeping existing fields.
- [x] PUB-T11. Keep readiness contract stable for existing consumers (without legacy execution paths):
  - `runtime_present` and `start_sh_executable` remain present for all versions.
  - No alternate/legacy runtime resolution logic is retained; only response-field continuity is preserved.

### Phase 3 — Config Assembly

- [x] PUB-T12. Implement config template loading from package (`config/default-config.json` or `config_template` field).
- [x] PUB-T13. Merge order at spawn:
  - template defaults
  - request `config` overrides
  - orchestrator `_system` overwrite
- [x] PUB-T14. Inject additional `_system` metadata for non-full runtimes:
  - `runtime_base`
  - `package_path`
- [x] PUB-T15. Preserve existing identity/system fields currently injected by orchestrator.

### Phase 4 — `fluxbee-publish` CLI

- [x] PUB-T16. Create CLI binary `fluxbee-publish` in Rust (new `src/bin/fluxbee_publish.rs` or dedicated crate).
- [x] PUB-T17. Implement package validation:
  - validate `package.json` fields and type-specific structure.
  - enforce runtime naming/format policy.
- [x] PUB-T18. Implement install step:
  - copy package to `/var/lib/fluxbee/dist/runtimes/<name>/<version>/`.
  - normalize permissions.
  - atomic manifest update.
- [x] PUB-T19. Implement `--dry-run`.
- [x] PUB-T20. Implement `--version <override>`.
- [x] PUB-T21. Implement `--deploy <hive>`:
  - fetch manifest version/hash from local versions endpoint.
  - call `sync-hint` + `update`.
  - report `ok`/`sync_pending`/`error`.

### Phase 5 — Core E2E Matrix

- [x] PUB-T22. E2E `full_runtime`: publish -> deploy -> spawn -> running.
  - Script: `scripts/runtime_packaging_pub_t22_e2e.sh`
- [x] PUB-T23. E2E `config_only`: publish base fixture -> publish config_only -> deploy -> spawn using base runtime.
  - Script: `scripts/runtime_packaging_pub_t23_e2e.sh`
- [x] PUB-T24. E2E `workflow`: publish engine fixture -> publish workflow package -> deploy -> spawn using workflow base runtime.
  - Script: `scripts/runtime_packaging_pub_t24_e2e.sh`
  - Self-contained; does not depend on pre-existing `wf.engine` on the target hive.
- [x] PUB-T25. E2E negative matrix:
  - missing `runtime_base`
  - unknown `runtime_base`
  - base runtime without executable `start.sh`
  - Script: `scripts/runtime_packaging_pub_t25_e2e.sh`
- [x] PUB-T26. E2E template layering:
  - template defaults applied
  - request overrides take precedence
  - `_system` fields forced by orchestrator
  - Validated on `config_only`.
  - Script: `scripts/runtime_packaging_pub_t26_e2e.sh`

### Phase 6 — Rollout and Documentation

- [ ] PUB-T27. Document rollout order:
  - orchestrator compatibility first
  - then CLI manifest v2 writes
  - then config-only/workflow packages
- [ ] PUB-T28. Update operator docs (`README.md` + runtime lifecycle runbook) replacing manual manifest editing with `fluxbee-publish`.
- [ ] PUB-T29. Add troubleshooting section:
  - common errors and remediation (`BASE_RUNTIME_NOT_PRESENT`, `MISSING_RUNTIME_BASE`, `sync_pending`).

---

## 12. Future: HTTP Upload (Deferred)

When the system needs to accept packages from remote callers (AI architect node, CI/CD pipeline), an HTTP upload endpoint will be added to SY.admin:

```
POST /runtimes/publish
Content-Type: multipart/form-data
Body: zip file + metadata
```

This endpoint will unpack the zip, validate the same way as the CLI, and call the same install logic. The CLI and the HTTP endpoint will share the validation and installation code.

For now, the CLI is sufficient for development and piloting.

---

## 13. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `runtime-lifecycle-spec.md` | CLI automates stages 1-2 (publish + manifest update). Stage 3-4 unchanged |
| `software-distribution-spec.md` | Dist layout extended with package.json and three types |
| `node-spawn-config-spec.md` | Config layering at spawn uses template from package |
| `system-inventory-spec.md` | Published runtimes visible in inventory after deploy |
| `admin-internal-gateway-spec.md` | Future: AI architect can publish via ADMIN_COMMAND + staging |
