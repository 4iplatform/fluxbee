# Runtime Lifecycle — Publish, Distribute, Update, Spawn

**Status:** v1.1
**Date:** 2026-03-12
**Audience:** Developers implementing orchestrator, admin API, and runtime management
**Parent specs:** `14-runtime-rollout-motherbee.md`, `software-distribution-spec.md`, `node-spawn-config-spec.md`
**Resolves:** FR-08 (manifest-vs-artifact inconsistency)

---

## 1. Problem

A runtime goes through four stages before a node can use it: publish (create artifacts on motherbee), distribute (replicate to workers), update (worker confirms readiness), and spawn (execute the node). Today these stages are partially documented across multiple specs, and there is a gap between stages 2 and 4: the manifest can arrive before the binary artifacts, causing the versions API to report a runtime as available when it cannot actually be executed.

This document defines the canonical lifecycle with clear contracts at each stage, ensuring that no runtime is reported as ready unless it can actually run.

---

## 2. The Four Stages

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐     ┌─────────────┐
│  1. PUBLISH  │────►│ 2. DISTRIBUTE│────►│  3. UPDATE   │────►│  4. SPAWN   │
│  (motherbee) │     │ (syncthing)  │     │  (worker)    │     │  (worker)   │
└─────────────┘     └──────────────┘     └─────────────┘     └─────────────┘
   Artifact +          Replication          Manifest +          Config +
   manifest            of dist/             artifact            execute
   in dist/            folder               verification        start.sh
```

**Invariant:** A node CANNOT be spawned on a worker unless that worker has completed stage 3 (UPDATE) with status `ok` for the target runtime and version. This is the core guarantee that eliminates FR-08.

---

## 3. Stage 1 — PUBLISH (Motherbee)

### 3.1 What Happens

The architect (human or AI) places the runtime artifacts on motherbee and updates the manifest.

### 3.2 Artifacts

```
/var/lib/fluxbee/dist/runtimes/
├── manifest.json
└── <runtime>/
    └── <version>/
        └── bin/
            └── start.sh          # Required entry point
```

### 3.3 Manifest Structure

```json
{
  "schema_version": 1,
  "version": 1710000000000,
  "updated_at": "2026-03-12T10:00:00Z",
  "runtimes": {
    "ai.soporte": {
      "available": ["1.0.0", "1.1.0", "1.2.0"],
      "current": "1.2.0"
    },
    "io.whatsapp": {
      "available": ["1.0.0"],
      "current": "1.0.0"
    }
  }
}
```

- `version`: monotonic timestamp (epoch ms). Used by SYSTEM_UPDATE to reject stale updates.
- `current`: the version that `"current"` resolves to in spawn requests.
- `available`: all versions with artifacts present in dist.

### 3.4 Publish Contract

A runtime version is considered published when BOTH conditions are met:
1. The version appears in `manifest.json` under `runtimes.<name>.available`.
2. The file `dist/runtimes/<name>/<version>/bin/start.sh` exists and is executable.

**Publish validation (recommended, requires FR8-T3/T4 implemented):**

The following validation uses the `readiness` block in the versions API, which is part of the target state defined in this spec. Until FR8-T3/T4 are implemented, verify artifact presence manually on disk.

```bash
# After placing artifacts and updating manifest:
curl -sS "$BASE/hives/$MOTHER_HIVE/versions" | python3 -c "
import json, sys
d = json.load(sys.stdin)
rt = d['payload']['hive']['runtimes']['runtimes']['$RUNTIME']
v = '$VERSION'
assert v in rt['available'], f'{v} not in available'
readiness = rt.get('readiness', {}).get(v, {})
assert readiness.get('runtime_present', False), f'{v} artifact not present locally'
assert readiness.get('start_sh_executable', False), f'{v} start.sh not executable'
print('PUBLISH OK')
"
```

**Interim validation (before FR8-T3/T4):**

```bash
# Check manifest entry
curl -sS "$BASE/hives/$MOTHER_HIVE/versions" | python3 -c "
import json, sys
d = json.load(sys.stdin)
rt = d['payload']['hive']['runtimes']['runtimes']['$RUNTIME']
assert '$VERSION' in rt['available'], 'not in manifest'
print('MANIFEST OK')
"
# Check artifact on disk
test -x "/var/lib/fluxbee/dist/runtimes/$RUNTIME/$VERSION/bin/start.sh" && echo "ARTIFACT OK" || echo "ARTIFACT MISSING"
```

---

## 4. Stage 2 — DISTRIBUTE (Motherbee → Worker)

### 4.1 What Happens

Syncthing replicates the `/var/lib/fluxbee/dist/` folder from motherbee to workers. This includes both the manifest and the runtime artifacts.

### 4.2 Distribution is Eventual

Syncthing replication is not instantaneous. The manifest (small JSON) typically arrives before the binary artifacts (potentially large). This is the root cause of FR-08: the worker sees the manifest but doesn't have `start.sh` yet.

### 4.3 Verifying Convergence

Before proceeding to UPDATE, verify that Syncthing has finished replicating:

```bash
curl -sS -X POST "$BASE/hives/$TARGET_HIVE/sync-hint" \
  -H "Content-Type: application/json" \
  -d '{"channel":"dist","folder_id":"fluxbee-dist","wait_for_idle":true,"timeout_ms":30000}'
```

Response `status: ok` means Syncthing reports idle for the dist folder. Response `status: sync_pending` means replication is still in progress.

**Important:** `sync-hint ok` means Syncthing thinks it's done, but does not guarantee artifact integrity. Stage 3 (UPDATE) performs the definitive verification.

---

## 5. Stage 3 — UPDATE (Worker)

### 5.1 What Happens

The caller sends SYSTEM_UPDATE to the worker's orchestrator. The orchestrator verifies that both the manifest and the artifacts are present locally, with matching versions and hashes.

### 5.2 Request

```bash
curl -sS -X POST "$BASE/hives/$TARGET_HIVE/update" \
  -H "Content-Type: application/json" \
  -d "{
    \"category\": \"runtime\",
    \"manifest_version\": $MANIFEST_VERSION,
    \"manifest_hash\": \"$MANIFEST_HASH\"
  }"
```

### 5.3 What Orchestrator Checks

```
1. Local manifest exists at /var/lib/fluxbee/dist/runtimes/manifest.json
2. Local manifest hash == requested manifest_hash
3. Local manifest version == requested manifest_version
   - If manifest_version=0: skip version check (initial bootstrap)
   - If local version < requested: respond sync_pending (manifest not yet replicated)
   - If local version > requested: respond VERSION_MISMATCH (stale update rejected)
4. For each runtime/version in manifest marked as "current":
   - /var/lib/fluxbee/dist/runtimes/<runtime>/<version>/bin/start.sh exists
   - start.sh is executable
```

**Note on current implementation (as of 2026-03-12):** Step 4 (artifact verification during UPDATE) is NOT yet implemented. Today UPDATE only validates manifest version and hash. Artifact presence is checked later during spawn (preflight). This spec defines the target state where UPDATE also verifies artifacts, so that `ok` from UPDATE is a reliable gate for spawn readiness.

### 5.4 Responses

| Status | Meaning | Caller action |
|--------|---------|---------------|
| `ok` | Manifest and all current artifacts verified locally | Proceed to spawn |
| `sync_pending` | Manifest or artifacts not yet present, or version not yet replicated | Wait, retry sync-hint, retry update |
| `error` (`VERSION_MISMATCH`) | Requested version is stale (local already ahead) | Use current version from motherbee |
| `error` (other) | Manifest invalid, hash corrupted, or other failure | Investigate, do not proceed |

**Key rule:** `ok` from UPDATE means "this worker can execute any runtime marked as current in the manifest." This is the gate that authorizes spawn.

### 5.5 Version Matching Semantics

The version check uses exact match, not >=. This is intentional:

- `manifest_version=0` → skip version check (bootstrap/recovery).
- `local == requested` → proceed to hash and artifact verification.
- `local < requested` → manifest hasn't arrived yet → `sync_pending`.
- `local > requested` → caller is behind → `status: error, error_code: VERSION_MISMATCH`.

---

## 6. Stage 4 — SPAWN (Worker)

### 6.1 What Happens

The caller requests a node to be spawned on the worker. Orchestrator performs a local preflight check on the specific runtime/version, creates config.json, and executes start.sh.

### 6.2 Request

```bash
curl -sS -X POST "$BASE/hives/$TARGET_HIVE/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\": \"AI.soporte.l1\",
    \"runtime\": \"ai.soporte\",
    \"runtime_version\": \"current\",
    \"config\": { ... }
  }"
```

### 6.3 Preflight Check (Orchestrator)

Before creating config.json or executing anything:

```rust
fn preflight_runtime(runtime: &str, version: &str) -> Result<PathBuf> {
    let resolved_version = if version == "current" {
        manifest.runtimes[runtime].current.clone()
    } else {
        version.to_string()
    };

    let start_sh = PathBuf::from("/var/lib/fluxbee/dist/runtimes")
        .join(runtime)
        .join(&resolved_version)
        .join("bin/start.sh");

    if !start_sh.exists() {
        return Err(SpawnError::RuntimeNotPresent {
            runtime: runtime.to_string(),
            version: resolved_version,
            expected_path: start_sh.display().to_string(),
            hint: "Run SYSTEM_UPDATE to materialize this runtime on the worker".to_string(),
        });
    }

    if !is_executable(&start_sh) {
        return Err(SpawnError::RuntimeNotExecutable {
            path: start_sh.display().to_string(),
        });
    }

    Ok(start_sh)
}
```

### 6.4 Error Response (RUNTIME_NOT_PRESENT)

```json
{
  "status": "error",
  "error_code": "RUNTIME_NOT_PRESENT",
  "error_detail": {
    "runtime": "ai.soporte",
    "version": "1.2.0",
    "expected_path": "/var/lib/fluxbee/dist/runtimes/ai.soporte/1.2.0/bin/start.sh",
    "hint": "Run SYSTEM_UPDATE to materialize this runtime on the worker"
  }
}
```

The error is accionable: the caller knows exactly what's missing and what to do.

### 6.5 Spawn Does NOT Auto-Update

If the artifact is missing, spawn fails. It does not attempt SYSTEM_UPDATE internally. The decision to sync and update is the caller's responsibility. This keeps spawn deterministic and avoids hidden side effects.

---

## 7. Versions API — Readiness Contract

### 7.1 Current Problem

`GET /hives/{hive}/versions` reads the manifest and reports runtimes as available. It does not check whether the local artifacts exist. This creates false positives.

### 7.2 New Contract

The versions endpoint reports both manifest data AND local readiness per runtime/version:

```json
{
  "status": "ok",
  "payload": {
    "hive_id": "worker-220",
    "runtimes": {
      "manifest_version": 1710000000000,
      "manifest_hash": "sha256:abc123...",
      "runtimes": {
        "ai.soporte": {
          "available": ["1.0.0", "1.1.0", "1.2.0"],
          "current": "1.2.0",
          "readiness": {
            "1.0.0": { "runtime_present": true, "start_sh_executable": true },
            "1.1.0": { "runtime_present": true, "start_sh_executable": true },
            "1.2.0": { "runtime_present": false, "start_sh_executable": false }
          }
        },
        "io.whatsapp": {
          "available": ["1.0.0"],
          "current": "1.0.0",
          "readiness": {
            "1.0.0": { "runtime_present": true, "start_sh_executable": true }
          }
        }
      }
    }
  }
}
```

**Fields per version in readiness:**

| Field | Type | Meaning |
|-------|------|---------|
| `runtime_present` | bool | `bin/start.sh` exists at the expected local path |
| `start_sh_executable` | bool | `bin/start.sh` has execute permission |

### 7.3 Interpretation Rules

| `runtime_present` | `start_sh_executable` | Can spawn? | Action needed |
|---|---|---|---|
| true | true | Yes | None |
| true | false | No | Fix permissions |
| false | false | No | Run sync-hint + SYSTEM_UPDATE |

### 7.4 Implementation

Orchestrator checks the filesystem for each runtime/version listed in the manifest:

```rust
fn check_readiness(runtime: &str, version: &str) -> Readiness {
    let start_sh = PathBuf::from("/var/lib/fluxbee/dist/runtimes")
        .join(runtime)
        .join(version)
        .join("bin/start.sh");

    Readiness {
        runtime_present: start_sh.exists(),
        start_sh_executable: start_sh.exists() && is_executable(&start_sh),
    }
}
```

---

## 8. Complete Flow — End to End

### 8.1 Happy Path

```
Architect                   Motherbee                    Worker
   │                           │                            │
   │  1. Place artifacts       │                            │
   │  2. Update manifest       │                            │
   │                           │                            │
   │  3. GET /versions         │                            │
   │  ◄── runtime_present:true │                            │
   │                           │                            │
   │                           │── Syncthing replicates ──►│
   │                           │                            │
   │  4. POST /sync-hint       │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄─────────────────── ok ──────────────────────────────│
   │                           │                            │
   │  5. POST /update          │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄─────────────────── ok ──────────────────────────────│
   │                           │                            │
   │  6. GET /versions (worker)│                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄── runtime_present:true ─────────────────────────────│
   │                           │                            │
   │  7. POST /nodes           │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄─────────────────── ok (node spawned) ───────────────│
```

### 8.2 Sync Not Ready

```
Architect                   Motherbee                    Worker
   │                           │                            │
   │  1-2. Publish on mother   │                            │
   │                           │── Syncthing in progress ──►│
   │                           │                            │
   │  3. POST /sync-hint       │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄──────────────── sync_pending ───────────────────────│
   │                           │                            │
   │  (wait, retry)            │                            │
   │                           │                            │
   │  4. POST /sync-hint       │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄─────────────────── ok ──────────────────────────────│
   │                           │                            │
   │  5. POST /update          │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄─────────────────── ok ──────────────────────────────│
   │                           │                            │
   │  6. POST /nodes           │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄─────────────────── ok ──────────────────────────────│
```

### 8.3 Manifest Arrived, Artifact Didn't

```
Architect                   Motherbee                    Worker
   │                           │                            │
   │  1-2. Publish on mother   │                            │
   │                           │── manifest arrived ───────►│
   │                           │── artifacts still syncing─►│
   │                           │                            │
   │  3. GET /versions (worker)│                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄── runtime_present:false ────────────────────────────│
   │                           │                            │
   │  (knows artifact is missing, waits for sync)           │
   │                           │                            │
   │  4. POST /sync-hint       │                            │
   │  ──────────────────────────────────────────────────────►│
   │  ◄──────────────── sync_pending ───────────────────────│
   │                           │                            │
   │  (retry loop until ok)    │                            │
   │                           │                            │
   │  5. POST /update → ok     │                            │
   │  6. POST /nodes  → ok     │                            │
```

### 8.4 Spawn Without Update (Error)

```
Architect                   Worker
   │                            │
   │  POST /nodes               │
   │  ─────────────────────────►│
   │                            │  preflight: start.sh missing
   │  ◄── RUNTIME_NOT_PRESENT ──│
   │       runtime: ai.soporte  │
   │       version: 1.2.0       │
   │       hint: run UPDATE     │
```

---

## 9. Current State vs Target State

| Area | Current state (2026-03-12) | Target state (this spec) |
|------|---------------------------|--------------------------|
| `GET /versions` readiness | Not implemented. Returns manifest only | Returns manifest + `readiness` per runtime/version with `runtime_present` and `start_sh_executable` |
| `SYSTEM_UPDATE` artifact check | Not implemented. Only validates manifest version/hash | Also verifies `start.sh` exists and is executable for each current runtime |
| Version semantics in UPDATE | Exact match or `manifest_version=0`; cualquier mismatch responde `sync_pending` | Mantiene exact match para `==` y `manifest_version=0`; distingue `local > requested` como `status:error` + `error_code=VERSION_MISMATCH` |
| Executable check in spawn | Checks `is_file` only, not executable bit | Checks both `exists` and `is_executable` |
| Spawn preflight error detail | Returns `RUNTIME_NOT_PRESENT` (basic) | Returns `RUNTIME_NOT_PRESENT` with runtime, version, expected_path, and hint |

---

## 10. Implementation Tasks (FR-08 Alignment)

### Phase 1 — Contract Definition

- [ ] FR8-T1. Adopt this document as the canonical runtime lifecycle spec.
- [ ] FR8-T2. Define flag semantics:
  - `runtime_present` = `bin/start.sh` exists at the expected local path.
  - `start_sh_executable` = `bin/start.sh` has execute permission.
  - Both must be `true` for a runtime to be spawn-ready.

### Phase 2 — Versions API

- [x] FR8-T3. Add `readiness` block per runtime/version in `GET /hives/{hive}/versions` response.
- [x] FR8-T4. Orchestrator checks filesystem for each version when building versions response.
- [ ] FR8-T5. Maintain manifest visibility regardless of readiness (manifest data always shown, readiness is additional).

### Phase 3 — UPDATE Hardening

- [x] FR8-T6. Add artifact verification in SYSTEM_UPDATE handler: for each current runtime/version, check `start.sh` exists and is executable.
- [x] FR8-T7. If artifacts missing during UPDATE, respond `sync_pending` (not `ok`).

### Phase 4 — Spawn Hardening

- [x] FR8-T8. Add explicit preflight check in `run_node`: verify `start.sh` exists AND is executable, before config.json creation.
- [x] FR8-T9. Return structured `RUNTIME_NOT_PRESENT` error with runtime, version, expected_path, and hint.
- [ ] FR8-T10. Spawn does NOT auto-trigger SYSTEM_UPDATE. Fails explicitly.

### Phase 5 — E2E Validation

- [ ] FR8-T11. E2E case A: runtime in manifest without `start.sh` → versions shows `runtime_present: false`, spawn returns `RUNTIME_NOT_PRESENT`.
- [ ] FR8-T12. E2E case B: after sync-hint + update → versions shows `runtime_present: true`, spawn succeeds.
- [ ] FR8-T13. E2E case C: spawn without prior update → `RUNTIME_NOT_PRESENT` with actionable hint.
- [x] FR8-T14. E2E case D: UPDATE with missing artifact → responds `sync_pending`, not `ok`.

### Phase 6 — Documentation

- [ ] FR8-T15. Update `07-operaciones.md` with canonical publish → distribute → update → spawn flow.
- [ ] FR8-T16. Update `14-runtime-rollout-motherbee.md` to reference readiness check in versions.

---

## 11. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `14-runtime-rollout-motherbee.md` | Operational runbook for the publish + update flow. Should reference this spec for the canonical lifecycle |
| `software-distribution-spec.md` | Defines dist layout and Syncthing sync. Stage 2 of this lifecycle |
| `node-spawn-config-spec.md` | Stage 4 details: config.json creation, spawn flow |
| `SY.orchestrator v2` | Orchestrator implements stages 3 and 4 |
| `system-inventory-spec.md` | Inventory shows nodes post-spawn; does not cover runtime readiness |
