# Policy + Solution Integration Tasks

**Status:** planning / pre-implementation  
**Date:** 2026-04-05  
**Source docs reviewed:**
- [`fluxbee_identity_claims_normative_model.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/TODO/fluxbee_identity_claims_normative_model.md)
- [`sy-policy-beta.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/TODO/sy-policy-beta.md)
- [`solution-manifest-spec_1.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/TODO/solution-manifest-spec_1.md)

**Canonical direction confirmed (2026-04-05):**
- `SY.policy` replaces `SY.claims` completely.
- The manifest spec to use for this initiative is [`solution-manifest-spec_1.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/TODO/solution-manifest-spec_1.md).
- [`docs/legacy/solution-manifest-spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/legacy/solution-manifest-spec.md) must be treated as historical only.

**Important scope decision:**
- Do **not** do the large Identity v3 migration now.
- `SY.policy` v1 must work on top of **Identity v2 current runtime data**:
  - `registration_status`
  - `tenant_id`
  - `ilk_type`
  - `roles[]`
  - `capabilities[]`
- Structured claims (`claims[]`, `recognition`, `institutional_role`) remain a later extension.

---

## 1) Main findings from review

### 1.1 What can be built now

- `SY.policy` is implementable now without Identity v3.
- The beta policy model already defines a valid v1 basis:
  - matrix = `claim × action_class × effect`
  - runtime matching can use identity v2 flat fields
  - OPA can consume compiled base rules plus a future override table
- The solution manifest can already adopt `policy_matrix` as the canonical normative section.

### 1.2 What must wait

- The identity normative model proposes a richer future identity:
  - `claims[]`
  - declaration vs recognition
  - `institutional_role`
  - `charter_ref`
- That is **not** required to launch `SY.policy` v1.
- If forced now, it would become an Identity v3 migration and blow up scope.

### 1.3 Spec friction that must be fixed first

- [`docs/legacy/solution-manifest-spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/legacy/solution-manifest-spec.md) still speaks in old `SY.claims` language.
- [`solution-manifest-spec_1.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/TODO/solution-manifest-spec_1.md) already moved to `SY.policy` + `policy_matrix`.
- For this initiative, the canonical manifest direction is already chosen: use `solution-manifest-spec_1.md` and treat the legacy file as historical.

### 1.4 Biggest implementation gap

The biggest missing technical piece is not identity itself. It is:
- defining a **canonical action classification model** from real runtime events/messages into `action_class`
- and defining which effects are truly enforceable now in OPA/router/admin

Without that, `policy_matrix` stays elegant but underspecified for code.

---

## 2) Phase split

### Phase A — Do now

Build `SY.policy` v1 using Identity v2 flat claims.

### Phase B — Do later

Evolve identity toward the richer claims model:
- `claims[]`
- recognition
- institutional role
- normative references

`SY.policy` must be prepared to extend to that, but not blocked by it now.

---

## 3) Immediate backlog

### POL-S0 — Canonicalize the specs first

- [x] POL-S0-T1. Choose canonical policy terminology:
  - `SY.policy` replaces `SY.claims`
  - `policy_matrix` replaces old claims-rule wording
- [x] POL-S0-T2. Canonicalize manifest direction:
  - [`solution-manifest-spec_1.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/TODO/solution-manifest-spec_1.md) is the manifest spec to use for this initiative
  - [`docs/legacy/solution-manifest-spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/legacy/solution-manifest-spec.md) remains historical and must not drive new implementation
- [x] POL-S0-T3. Add explicit compatibility note:
  - `SY.policy` v1 runs on Identity v2 flat metadata
  - Identity v3 claims are deferred
  - only forward-compatible hooks that reduce later migration cost should be prepared now
- [x] POL-S0-T4. Freeze vocabulary:
  - `claim matcher`
  - `action_class`
  - `effect`
  - `override`
  - `reincidence`
  - any old `SY.claims` wording is historical only

### POL-S1 — Freeze `policy_matrix` v1 against Identity v2

- [ ] POL-S1-T1. Define the exact allowed claim matcher keys for v1:
  - `registration_status`
  - `tenant_id`
  - `ilk_type`
  - `role`
  - `capability`
- [ ] POL-S1-T2. Explicitly reject v3-only matcher fields for now:
  - `claims[].facet`
  - `recognized_by`
  - `institutional_role.*`
  - `charter_ref` as matcher input
- [ ] POL-S1-T2b. Define forward-compatible hooks for Identity v3 without migrating now:
  - keep room in the matcher/compiler for future structured-claim adapters
  - allow rule metadata to carry non-executable references such as `charter_ref`
  - do not require `claims[]` / `recognized_by` / `institutional_role` in runtime evaluation yet
- [ ] POL-S1-T3. Define matcher semantics:
  - scalar equality
  - array contains for `role` / `capability`
  - wildcard support or explicit no-wildcard rule
- [ ] POL-S1-T4. Define rule priority / tie-breaking:
  - more specific beats less specific
  - deny beats allow
  - override beats base rule
- [ ] POL-S1-T5. Define default behavior when no rule matches:
  - keep current proposal `allow`
  - or tighten selected action classes only

### POL-S2 — Define canonical `action_class` mapping

- [ ] POL-S2-T1. Define the runtime source of truth for classification inputs:
  - `msg_type`
  - admin action
  - node control action
  - destination/type context
  - result status
- [ ] POL-S2-T2. Produce a first deterministic classifier for:
  - `send_message`
  - `read`
  - `write`
  - `system_config`
  - `topology_change`
  - `external_action`
  - `identity_change`
  - `workflow_step`
  - `node_lifecycle`
- [ ] POL-S2-T3. Define where this classifier runs:
  - router helper
  - `SY.policy`
  - shared library/helper
- [ ] POL-S2-T4. Document ambiguous cases:
  - admin read vs node control read
  - config set vs node spawn
  - workflow internal messages vs user/system messages

### POL-S3 — Freeze v1 effect set

- [ ] POL-S3-T1. Decide which effects are enforceable now in the live stack:
  - `allow`
  - `deny`
  - `route_to_frontdesk`
  - maybe `route_to`
- [ ] POL-S3-T2. Defer effects that need extra product plumbing:
  - `require_confirmation`
  - `flag` as real runtime behavior
- [ ] POL-S3-T3. For deferred effects, define fallback behavior:
  - accepted in spec but not executable yet
  - or forbidden in v1 schema

### POL-S4 — OPA integration shape

- [ ] POL-S4-T1. Define deterministic compiler from `policy_matrix` to Rego/WASM input.
- [ ] POL-S4-T2. Decide whether base rules still live only in `jsr-opa` compiled WASM or also as structured matrix payload for `SY.policy`.
- [ ] POL-S4-T3. Define override precedence contract in OPA.
- [ ] POL-S4-T4. Define override expiry semantics and time source.

### POL-S5 — `jsr-policy` SHM

- [ ] POL-S5-T1. Define `jsr-policy-<hive>` region layout.
- [ ] POL-S5-T2. Define override entry schema:
  - ids
  - subject ILK
  - action_class
  - effect
  - severity
  - TTL/expiry
  - source rule/case
- [ ] POL-S5-T3. Define capacity and eviction/failure behavior.
- [ ] POL-S5-T4. Define read path for OPA/router and write path for `SY.policy`.

### POL-S6 — `SY.policy` node v1

- [ ] POL-S6-T1. Create `SY.policy` node skeleton:
  - config
  - lifecycle
  - NATS consumer
  - status/config-get
- [ ] POL-S6-T2. Consume the right event stream:
  - likely `storage.turns`
  - optionally admin/system events if needed
- [ ] POL-S6-T3. Build normative case from:
  - actor ILK
  - identity snapshot
  - action_class
  - actual outcome
- [ ] POL-S6-T4. Evaluate case against matrix.
- [ ] POL-S6-T5. Write overrides to `jsr-policy`.
- [ ] POL-S6-T6. Persist cases / audit trail via `SY.storage`.

### POL-S7 — Admin and Architect integration

- [ ] POL-S7-T1. Add admin actions:
  - `list_policy_overrides`
  - `clear_override`
  - `list_policy_matrix`
  - `update_policy_matrix`
  - `list_policy_violations`
- [ ] POL-S7-T2. Update Archi SCMD/help/prompts for the policy surface.
- [ ] POL-S7-T3. Define confirmation rules for policy mutations.

### POL-S8 — Solution manifest integration

- [ ] POL-S8-T1. Make `policy.policy_matrix` the canonical normative block.
- [ ] POL-S8-T2. Remove old `SY.claims` wording from specs used by new implementation.
- [ ] POL-S8-T3. Define manifest validation for:
  - policy_matrix schema
  - route references
  - runtime/node references
  - tenant/hive consistency
- [ ] POL-S8-T4. Define reconciliation order for policy:
  - compile/apply OPA
  - load matrix reference for `SY.policy`
  - verify policy active

### POL-S9 — Reconciliation engine in Architect

- [ ] POL-S9-T1. Read actual state:
  - inventory
  - versions
  - nodes
  - routes
  - OPA status
  - policy status
- [ ] POL-S9-T2. Build manifest diff engine.
- [ ] POL-S9-T3. Build reconciliation executor with confirmation gates.
- [ ] POL-S9-T4. Add manifest history + reconciliation logs.

---

## 4) Deferred explicitly to Identity v3

These should remain documented, but not implemented in the first `SY.policy` wave:

- [ ] IDV3-T1. Add structured `claims[]` to ILK metadata.
- [ ] IDV3-T2. Add declaration vs recognition model.
- [ ] IDV3-T3. Add `institutional_role`.
- [ ] IDV3-T4. Add `charter_ref` / normative refs on identity.
- [ ] IDV3-T5. Extend `policy_matrix` matcher to consume structured claims.

Rule:
- no runtime dependency of `SY.policy` v1 on these fields
- only forward-compatible schema planning

---

## 5) Recommended implementation order

### Order 1 — safest

1. Canonicalize spec language and active docs.
2. Freeze `policy_matrix` v1 on top of Identity v2.
3. Freeze `action_class` mapping and effect set.
4. Implement compiler + `jsr-policy`.
5. Create `SY.policy`.
6. Integrate admin/architect.
7. Integrate solution manifest reconciliation.

### Order 2 — things to avoid now

- Do not start by changing Identity to structured claims.
- Do not start by building the full manifest triunvirate flow.
- Do not start by designing judicial/governance complexity beyond overrides + pardon + audit.

---

## 6) Concrete recommendation

If we want the first useful milestone quickly, the next execution slice should be:

- [ ] Slice P1. Spec consolidation:
  - active manifest spec aligned to `SY.policy`
  - v1 matcher/effect/action_class frozen
- [ ] Slice P2. Core runtime:
  - `policy_matrix` compiler
  - `jsr-policy` SHM
  - OPA override consumption
- [ ] Slice P3. Service:
  - `SY.policy` post-facto evaluator on `storage.turns`
- [ ] Slice P4. Operations:
  - Admin + Archi policy actions

This gets us a real normative engine without paying the full Identity v3 migration cost.
