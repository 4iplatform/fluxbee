# Runtime Package Publish / Materialize - Tasks

**Status:** active workstream  
**Date:** 2026-04-20  
**Audience:** platform developers, Archi designers, admin/orchestrator owners  
**Parent specs:** `runtime-lifecycle-spec.md`, `runtime-packaging-cli-spec.md`, `sy-architect-spec.md`, `sy-wf-rules-spec.md`, `docs/node-package-install-spec-TODO.md`

---

## 1. Goal

Replace the CLI-centered runtime publication flow with a canonical system capability that Archi can invoke directly.

This workstream covers the two creation paths that must end in the same place: a published runtime/package in `dist/runtimes` plus manifest entry, ready to go through `distribute -> update -> spawn`.

1. `inline_package`
   - Archi generates the package contents directly from structured data.
   - Primary use case: instance-oriented nodes whose runtime package is mostly config/assets/code generated from a manifest or execution plan.
2. `bundle_upload`
   - Archi/operator provides a `.zip` bundle containing the complete package.
   - Primary use case: `full_runtime` packages that ship their own binary/scripts.

The target operating model is:

- Archi orchestrates publish/deploy/spawn through system actions.
- `SY.admin` owns package publication on `motherbee`.
- `SY.orchestrator` continues to own `sync-hint`, `update`, `run_node`, `restart_node`, `kill_node`.
- `fluxbee-publish` becomes deprecated and stops being the canonical path.

---

## 2. Scope Decisions

Closed decisions for this workstream:

- There will be one canonical publish capability with two source modes:
  - `source.kind = "inline_package"`
  - `source.kind = "bundle_upload"`
- Publication is stage 1 of the lifecycle only:
  - validate package
  - install into `dist`
  - update `manifest.json`
  - optionally request `sync-hint + update`
- Spawn remains separate and keeps using existing node lifecycle APIs.
- `motherbee` is the only writer of `dist/runtimes` and `manifest.json`.
- The implementation must reuse one shared publish/install library. No duplicated validation logic between API, Archi helpers, `SY.wf-rules`, or deprecated CLI.
- `WF.*` authoring remains owned by `SY.wf-rules`.
  - `SY.wf-rules` may reuse the same shared publish/install library internally.
  - workflow authors should not bypass `SY.wf-rules` by uploading ad hoc workflow zips as the primary path.
- `fluxbee-publish` is deprecated once the API/system path is complete.
  - no new product features should be added only to the CLI
  - CLI may survive temporarily as a thin local wrapper over the shared library during migration

Out of scope for this workstream:

- inventing a second spawn model
- distributed atomicity across hives
- remote build pipelines
- replacing `SY.wf-rules` workflow lifecycle with raw package upload

---

## 3. Canonical End State

Archi or any authorized caller can request package publication through `SY.admin` using one canonical action:

- same validation rules for all package types
- same install semantics into `dist/runtimes/<name>/<version>/`
- same atomic manifest update
- same response shape for readiness/deploy follow-up

Source modes:

1. `inline_package`
   - caller sends structured package metadata plus file contents
   - system materializes a package staging directory, validates it, installs it, updates manifest
2. `bundle_upload`
   - caller references a staged blob or upload token for a `.zip`
   - system extracts, validates, installs, updates manifest

Optional follow-up operations after publish:

- `sync_hint` to selected hives
- `update(category=runtime)` to selected hives
- explicit `run_node`/`restart_node` in a later step of the plan

---

## 4. Delivery Phases

### Phase A - Contract and Ownership

- [ ] RPP-A1. Freeze the canonical publish contract in docs.
  - One action family, two source modes.
  - Publish is distinct from spawn.
  - `motherbee` is sole owner of `dist` writes.
- [ ] RPP-A2. Define the request schema for `inline_package`.
  - package metadata
  - package file map
  - optional `set_current`
  - optional `deploy_to` / `update_to`
- [ ] RPP-A3. Define the request schema for `bundle_upload`.
  - bundle locator
  - optional expected runtime name/version
  - optional `set_current`
  - optional `deploy_to` / `update_to`
- [ ] RPP-A4. Define the canonical response schema.
  - validated package metadata
  - install path
  - manifest version/hash
  - requested follow-up operations and their status
- [ ] RPP-A5. Define error codes for publication.
  - invalid package layout
  - validation failure
  - runtime base missing/unavailable
  - version already exists
  - busy/locked
  - source bundle missing
  - unsupported source kind

### Phase B - Shared Publish/Install Core

- [x] RPP-B1. Extract shared validation/install logic from `fluxbee-publish` into reusable library code.
  - Implemented in Rust as `src/runtime_package.rs`.
  - `fluxbee-publish` now consumes the shared core instead of owning validation/install logic inline.
- [ ] RPP-B2. Make shared code support both filesystem package dirs and extracted/materialized staging dirs.
- [x] RPP-B3. Keep one implementation for:
  - package validation
  - permission normalization
  - atomic install to `dist`
  - atomic manifest update
- [x] RPP-B4. Support manifest v2 fields consistently in shared code.
  - `type`
  - `runtime_base`
  - Shared core preserves existing manifest v2 gate/write behavior.
- [x] RPP-B5. Expose publish result as structured data instead of CLI-only text output.
  - Shared structs now expose validated package metadata, dry-run plan, and install result.
- [x] RPP-B6. Add idempotency/conflict behavior in shared core.
  - Shared core now treats `same version + same normalized contents` as idempotent success.
  - Shared core now rejects `same version + different contents` as an explicit install conflict.

### Phase C - `SY.admin` Publish Actions

- [x] RPP-C1. Add canonical admin action registry entries for package publication.
  - Added `publish_runtime_package` to `SY.admin` action registry/help catalog.
  - Added HTTP path `POST /admin/runtime-packages/publish`.
- [x] RPP-C2. Implement `inline_package` materialization path in `SY.admin`.
  - Creates staging dir under admin state.
  - Writes provided files with path traversal rejection.
  - Validates and installs through the shared runtime package core.
  - Current scope: `source.kind=inline_package`.
- [x] RPP-C3. Implement `bundle_upload` path in `SY.admin`.
  - Resolves `blob_path` relative to the configured blob root.
  - Extracts zip bundles into admin staging with traversal/layout checks.
  - Validates and installs through the shared runtime package core.
- [ ] RPP-C4. Enforce lifecycle serialization with the same runtime lifecycle lock semantics.
- [x] RPP-C5. Restrict package publication writes to `motherbee`.
  - `publish_runtime_package` rejects non-`motherbee` execution with `NOT_PRIMARY`.
- [x] RPP-C6. Return canonical admin envelope with publish details.
  - Success/error responses use the standard admin envelope with structured publish payload.
- [x] RPP-C7. Wire optional post-publish follow-up:
  - `sync_to` runs `sync_hint(channel=dist)` after publish.
  - `update_to` runs `update(category=runtime)` scoped to the new runtime/version.
  - No implicit spawn; follow-up results return in the same publish response payload.

### Phase D - Archi Integration

- [x] RPP-D1. Add Archi-facing tool/action for publishing runtime packages.
  - `sy_architect` translates `POST /admin/runtime-packages/publish` to `publish_runtime_package`.
  - The generic mutating admin tool allowlist now permits `publish_runtime_package`.
  - `SY.admin` executor pilot/subset catalog now includes `publish_runtime_package`, `sync_hint`, `update`, and `run_node` so plans can compose the lifecycle without extra catalog overrides.
- [x] RPP-D2. Support `inline_package` requests from manifests/execution plans.
  - `publish_runtime_package` is now a native executor-plan action with documented `inline_package` examples.
  - `sy_architect` now exposes `fluxbee_publish_runtime_package` for large inline file maps.
  - Package materialization can now be delegated internally to the new `infrastructure` specialist via `fluxbee_infrastructure_specialist`, producing an internal infrastructure artifact whose `payload.publish_request` feeds publish.
- [x] RPP-D3. Support `bundle_upload` requests from manifests/execution plans.
  - Executor-plan docs now include canonical `bundle_upload` step shape with `blob_path`.
  - `fluxbee_publish_runtime_package` also supports bundle sources via `source` / `source_json`.
- [x] RPP-D4. Define manifest/execution-plan task shape for publish.
  - The canonical plan step is `action = "publish_runtime_package"` with native admin args.
  - Shape is documented with `source.kind`, `sync_to`, and `update_to`.
- [x] RPP-D5. Ensure Archi composes the lifecycle explicitly:
  - Documented canonical sequence: publish -> declared sync/update intent -> run_node/restart_node.
  - No implicit spawn from publish success.
- [x] RPP-D6. Update solution-manifest/execution-plan docs to stop referencing `fluxbee-publish` as the operator path.
  - Executor-plan spec now documents `publish_runtime_package` as the canonical plan action.
  - Solution-manifest reconciliation docs now point to `publish_runtime_package` instead of `fluxbee-publish`.

### Phase E - Service Integration

- [ ] RPP-E1. Make `SY.wf-rules` reuse the shared publish/install core instead of owning a divergent install path.
- [ ] RPP-E2. Verify published workflow packages still bind cleanly through orchestrator package-aware spawn.
- [ ] RPP-E3. Verify non-WF instance-oriented packages produced by Archi can be spawned with existing orchestrator runtime-base logic.
- [ ] RPP-E4. Keep publish and node lifecycle responsibilities separate in code boundaries.

### Phase F - Tests

- [x] RPP-F1. Unit tests for shared validation/install core after extraction.
  - Added shared-core tests for idempotent re-publish and explicit conflict on divergent content.
- [x] RPP-F2. Admin tests for `inline_package`.
  - happy path
  - invalid file layout
  - conflict/idempotency
- [x] RPP-F3. Admin tests for `bundle_upload`.
  - missing bundle
  - invalid zip
  - invalid package layout
  - happy path for `full_runtime`
- [ ] RPP-F4. E2E publish -> sync/update -> spawn for:
  - `full_runtime` via `bundle_upload`
  - `config_only` via `inline_package`
- [ ] RPP-F5. E2E negative matrix.
  - missing runtime base
  - base runtime not ready
  - duplicate version with different content
  - publish while lifecycle lock is held
- [ ] RPP-F6. E2E Archi plan flow.
  - manifest/execution plan publishes package
  - deploys it
  - spawns/restarts managed node

### Phase G - Deprecation and Cleanup

- [ ] RPP-G1. Mark `fluxbee-publish` deprecated in docs and operator guidance.
- [ ] RPP-G2. Stop using `fluxbee-publish` as the canonical reference in examples/runbooks.
- [ ] RPP-G3. Decide whether CLI remains as:
  - thin wrapper over shared core during migration, or
  - fully retired binary
- [ ] RPP-G4. Update troubleshooting/runbooks to reference admin/Archi publish path.
- [ ] RPP-G5. Remove scattered “publish by API later” backlog items once this workstream is in progress.

---

## 5. Acceptance Criteria

This workstream is complete when all of the following are true:

1. Archi can publish a package without invoking `fluxbee-publish`.
2. Archi can publish both:
   - a structured/generated package
   - a zipped full runtime bundle
3. Publication ends in canonical `dist + manifest` state on `motherbee`.
4. Workers can then converge through the existing `sync-hint + update` flow.
5. Spawn/restart continues to use the existing orchestrator lifecycle path.
6. `SY.wf-rules` and generic package publication share the same core install logic.
7. Operator docs no longer present `fluxbee-publish` as the recommended primary path.

---

## 6. Backlog Absorbed Here

This document absorbs and replaces scattered pending items that should no longer live as separate loose ends:

- `docs/onworking COA/sdk_integration_tasks.md`
  - `Publish por API`
- `docs/runtime-packaging-cli-spec.md`
  - `PUB-T27`
  - `PUB-T28`
  - `PUB-T29`
  - deferred HTTP upload follow-up
- `docs/onworking COA/sy_admin_tasks.md`
  - remaining packaging/publication cleanup for `CUSTOM` managed runtimes

When work starts here, those items should be treated as superseded by this workstream, not tracked independently.
