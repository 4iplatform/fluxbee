# SY.policy — Decision Note for Implementation

**Date:** 2026-04-06
**Context:** Responses to programmer's 10 colisiones identified before starting SY.policy development
**Status:** Decisions closed, ready for implementation
**Reference docs:** `sy-policy-beta.md`, `solution-manifest-spec_1.md`, `fluxbee_identity_claims_normative_model.md`

---

## 1. "Claim" sobrecargado → Renombrar a `match`

**Decision:** The field in the policy_matrix that evaluates against identity data is called `match`, not `claim`.

```json
// Correcto (v1)
"match": { "registration_status": "temporary" }
"match": { "capability": "billing" }
"match": { "role": "support_l1" }

// Futuro (identity v3, sin cambio de estructura)
"match": { "claim.facet": "function", "claim.key": "handle_billing" }
```

`match` is a generic predicate evaluator over identity data. Today it evaluates flat fields from identity v2. When identity v3 adds structured claims, the same `match` evaluates deeper fields. The code is a predicate evaluator over a key-value map — it doesn't know which identity version is running.

**V1 constraints on `match`:**

- scalar equality only
- array-contains only for `role` / `capability`
- no generic wildcard matcher in v1

**Action:** Replace `claim` with `match` in all policy_matrix references. This change applies to `sy-policy-beta.md`, `solution-manifest-spec.md`, and any implementation code.

---

## 2. `action_class` — Quién lo clasifica

**Decision:** The point of entry classifies the action. It is deterministic, not AI. The classification lives as a canonical system contract, administered through code/admin surfaces, not as an ad-hoc static catalog file.

**Canonical v1 enum:**

- `send_message`
- `read`
- `write`
- `system_config`
- `topology_change`
- `external_action`
- `identity_change`
- `workflow_step`
- `node_lifecycle`

`action_class="*"` is not allowed in v1. The class space stays closed and explicit.

**Router classifies common routed messages:**

| Origin | Condition | `action_class` |
|--------|-----------|----------------|
| Carrier/router | normal user/app message | `send_message` |
| IO inbound | external channel enters Fluxbee | `send_message` |
| Node-to-node direct message | `send_node_message` | `send_message` |

**Admin classifies commands (by action field):**

| `action` | `action_class` |
|----------|----------------|
| `list_hives`, `get_hive`, `list_nodes`, `get_node_status`, `get_node_config`, `get_node_state`, `get_versions`, `inventory`, `opa_get_status`, `opa_get_policy` | `read` |
| `CONFIG_GET`, `STATUS`, `PING` | `read` |
| `set_node_config`, `node_control_config_set`, `CONFIG_SET` | `write` |
| `add_route`, `delete_route`, `add_vpn`, `delete_vpn`, `opa_apply`, `opa_compile_apply`, `opa_rollback`, `update_policy_matrix`, `clear_override`, `sync_hint`, `update` | `system_config` |
| `add_hive`, `remove_hive` | `topology_change` |
| `run_node`, `kill_node`, `remove_node_instance` | `node_lifecycle` |
| `create_tenant`, `update_tenant`, `approve_tenant`, `list_ilks`, `get_ilk`, `delete_ilk`, `list_vocabulary`, `add_vocabulary`, `deprecate_vocabulary` | `identity_change` |

**IO/WF classify explicit side effects:**

| Origin | Action type | `action_class` |
|--------|-------------|----------------|
| IO node sending to external API | WhatsApp send, email send | `external_action` |
| WF node executing step | workflow advance, trigger, complete | `workflow_step` |

**Action:** Router/admin/IO/WF must set `action_class` in the action metadata. If an event arrives at `SY.policy` without `action_class`, policy logs an error and skips evaluation — it does not guess.

### 2.1 `action_result` contract

**Decision:** `SY.policy` also needs the action outcome for all evaluable side-effect classes.

`action_result` is required for:

- `write`
- `system_config`
- `topology_change`
- `external_action`
- `identity_change`
- `workflow_step`
- `node_lifecycle`

`action_result` is optional for:

- `read`
- `send_message`

**Canonical v1 values:**

- `blocked`
- `applied`
- `failed`

**Normative reading:**

- `deny + applied` = real violation
- `deny + blocked` = correct enforcement
- `deny + failed` = not treated as violation by default in v1

**Additional outcome fields:**

- `result_origin`:
  - optional in general
  - required when `action_result=blocked`
  - tells whether the block came from `opa`, `node`, `external`, or another source
- `result_detail_code`:
  - optional
  - reserved for future distinction between technical failure and business rejection

### 2.2 Priority and collision rules

**Decision:** v1 keeps rule resolution simple and rejects ambiguity early.

Priority:

- override beats base rule
- more specific `match` beats less specific `match`
- specificity = number of predicates in `match`
- on same-specificity ties, `deny` beats `allow`

Compiler/apply collision rule:

- if two base rules have the same `action_class`
- and the same predicate count
- and they target the same normative space
- and they produce different effects

the matrix is rejected at apply time as ambiguous. Runtime evaluation does not try to guess which law should win.

---

## 3. Effects ejecutables en v1

**Decision:** v1 implements 3 effects. The other 3 are documented as future.

| Effect | V1 | Implementation |
|--------|-----|----------------|
| `allow` | ✓ | OPA default |
| `deny` | ✓ | OPA deny rule |
| `route_to_frontdesk` | ✓ | OPA routing rule (already exists) |
| `flag` | Future | Deferred until audit storage, visibility, and operator UX are designed cleanly |
| `require_confirmation` | Future | Needs confirmation flow in destination node |
| `route_to` | Future | Needs dynamic routing target in OPA |

**Action:** The JSON schema accepts all 6 values (forward-compatible). The matrix-to-Rego compiler only compiles rules with v1 effects. Rules with future effects are stored but not compiled — a warning is logged at compile time. This lets the Blueprint include future rules without breaking the system.

---

## 4. Una fuente, dos consumidores, dos niveles

**Decision:** The effective policy rule model is the single source of truth in runtime. It is composed from two inputs with the same rule shape:

1. **System axioms** — shipped with Fluxbee core as an immutable system-policy pack, installed by orchestrator into the `SY.architect` environment as a read-only source.
2. **Solution policy_matrix** — authored in the solution manifest.

The composed effective policy is consumed by two components:

1. **Matrix-to-Rego compiler** → produces `axioms.rego` + `solution.rego` → jsr-opa SHM (real-time enforcement)
2. **SY.policy** → persists and loads the effective policy matrix → evaluates post-facto

No semantic divergence should exist because both use the same composed rule model. The compiler is a deterministic if/then table — no room for semantic drift.

**Two levels within the same matrix:**

| Level | Field | Mutable | Policy can override | Source |
|-------|-------|---------|-------------------|--------|
| **Axiom** | `level: "axiom"` | Immutable at runtime | No | Fluxbee system policy pack shipped with the release |
| **Solution** | `level: "solution"` | Yes | Yes | Solution manifest / Blueprint |

Axioms live in the system, not in the Blueprint. They use the same JSON rule format as solution rules, but are stored separately as an immutable system-policy pack installed with Fluxbee and made available inside the `SY.architect` environment for composition. Solution rules live in the Blueprint and follow the deploy lifecycle (prepare → enable → rollback).

**Rego composition:**

```
OPA evaluates = axioms.rego (system, immutable)
              + solution.rego (Blueprint, stageable)
              + overrides data (SY.policy, dynamic)
```

ENABLE only swaps `solution.rego`. Axioms remain unchanged for normal solution deploys. Overrides persist across deploys.

---

## 5. Post-facto vs real-time — Qué va en base OPA

**Decision:** Aligned with point 4. Axioms are always in base OPA from system start. Solution rules go to base OPA at ENABLE time. Both are real-time enforcement.

The difference between axioms and solution rules is not "real-time vs post-facto" — both are real-time in OPA. The difference is:

| | Axioms | Solution rules |
|---|---|---|
| When loaded | From installed system policy pack | Blueprint ENABLE |
| Changed by | Fluxbee upgrade / rollback | Architect via Blueprint |
| Override by SY.policy | No | Yes |
| Rollback | No | Yes (restore previous solution.rego) |
| Example | "temporary can't system_config" | "billing agent can external_action" |

SY.policy evaluates post-facto against BOTH levels but can only produce overrides against solution rules.

**Operational clarification:** `SY.architect` is the canonical composer. It:

- reads the installed read-only system-axioms pack from its environment
- reads the solution manifest policy block
- composes the effective matrix
- generates/applys Rego for OPA
- provides the effective matrix to `SY.policy`, which persists it as its own evaluation reference

---

## 6. `charter_ref` — Metadata only

**Decision:** In v1, `charter_ref` is metadata only. It documents which charter or normative source a rule comes from. It is NOT a matching condition, NOT an input to OPA, NOT a runtime-resolvable reference.

```json
{
  "rule_id": "pm:020",
  "match": { "capability": "billing" },
  "action_class": "external_action",
  "effect": "allow",
  "level": "solution",
  "charter_ref": "charter://acme/support-v1",
  "description": "Billing agents can perform external actions"
}
```

`charter_ref` is for humans and the architect to know where the rule came from. The system ignores it for evaluation.

**Future:** When identity v3 has `institutional_role.charter_ref`, the match field could reference it: `"match": { "institutional_role.charter_ref": "charter://..." }`. But not now.

---

## 7. Override granularity — Per event, not per scope

**Decision:** Overrides are per `ilk_id + action_class`, with optional `tenant_id` for narrower scope. Not grouped, not scoped to broad categories.

```json
{
  "override_id": "ovr:uuid",
  "ilk_id": "ilk:uuid",
  "action_class": "external_action",
  "tenant_id": "tnt:uuid",
  "effect": "deny",
  "expires_at": "2026-04-30T00:00:00Z",
  "source_rule_id": "pm:020",
  "reincidence_level": 1
}
```

If `tenant_id` is null, the override applies across all tenants. If set, only for that tenant. This allows: "Juan can't do external_action in Acme tenant, but can in other tenants."

Each event (message/action) is evaluated individually. If a violation is detected, one override is produced for that specific ilk + action_class (+ optional tenant). The rest of the ILK's permissions remain untouched.

**Runtime note:** overrides are online runtime exceptions, not historical records. They must live in `jsr-policy` for fast evaluation and be restorable from local `SY.policy` persistence after restart. Historical/audit data can go to storage fire-and-forget, but storage is not the primary recovery source for active overrides.

---

## 8. Actor identity — `src_ilk` per event

**Decision:** SY.policy evaluates based on `src_ilk` — the ILK that appears as the sender of the message or the initiator of the admin command at the moment the action occurs.

No delegation tracing. No principal chains. No "who asked who to do what."

If IO.whatsapp forwards a human's message, `src_ilk` is the human's ILK.
If AI.soporte sends an admin command, `src_ilk` is AI.soporte's ILK.
If WF.escalation triggers a step, `src_ilk` is WF.escalation's ILK.

Each evaluation is per-message, per-event. Compound behavior analysis (patterns across messages) is handled by reincidence tracking over individual events, not by tracing action chains.

---

## 9. Deploy in phases — PREPARE → ENABLE → VALIDATE → ROLLBACK

**Decision:** Blueprint reconciliation runs in ordered phases. The key insight: new nodes run first without receiving traffic. OPA/matrix activation is atomic and separate.

```
Phase 1: PREPARE
  - Create tenant if missing
  - Add hives if missing
  - Publish runtimes
  - Sync runtimes to workers
  - Spawn NEW nodes (they run but OPA still routes to old nodes)
  - Compose effective policy = system axioms + solution policy_matrix
  - Prepare new solution.rego (compiled from solution policy_matrix)
  - Verify new nodes are healthy
  - NO traffic change yet

Phase 2: ENABLE
 - Swap solution.rego in jsr-opa SHM (atomic)
  - New routing rules take effect
  - Traffic flows to new nodes
  - Axioms unchanged, overrides unchanged
  - `SY.policy` receives/persists the new effective matrix for post-facto evaluation

Phase 3: VALIDATE
  - Architect verifies system is working
  - Can use IO impersonation to test flows
  - Monitor node health, errors, policy violations

Phase 4a: CLEANUP (if success)
  - Kill old nodes that are no longer in the Blueprint
  - Remove old runtimes if unused
  - Log successful deploy

Phase 4b: ROLLBACK (if failure)
  - Restore previous solution.rego in jsr-opa SHM (atomic)
  - Traffic returns to old nodes (they're still running)
  - Kill new nodes
  - Log failed deploy with reason
```

**Key properties:**

- Axioms are never touched by deploy.
- Old nodes keep running until CLEANUP — rollback is instant because there's nothing to rebuild.
- The ENABLE is just a SHM swap — microseconds.
- Testing before ENABLE: architect uses IO impersonation to send messages directly to new nodes (bypassing OPA routing). If they respond correctly, ENABLE.

**Action:** Update `solution-manifest-spec_1.md` reconciliation section to reflect this phased model and the installed system-axioms composition.

---

## 10. Identity v3 forward-compat

**Decision:** The `match` evaluator is a generic predicate over a key-value map of identity fields. Implementation does not hardcode which fields exist.

```rust
// The matcher receives a map, not a struct
fn evaluate_match(
    match_predicates: &HashMap<String, Value>,
    identity_data: &HashMap<String, Value>,
) -> bool {
    match_predicates.iter().all(|(key, expected)| {
        identity_data.get(key)
            .map(|actual| matches_value(actual, expected))
            .unwrap_or(false)
    })
}
```

Today `identity_data` is populated from identity v2 flat fields:

```rust
let mut identity_data = HashMap::new();
identity_data.insert("registration_status", ilk.registration_status);
identity_data.insert("tenant_id", ilk.tenant_id);
identity_data.insert("ilk_type", ilk.ilk_type);
for cap in &ilk.capabilities {
    identity_data.insert("capability", cap);  // multi-value
}
for role in &ilk.roles {
    identity_data.insert("role", role);  // multi-value
}
```

When identity v3 arrives:

```rust
// Just add more fields to the same map
for claim in &ilk.claims {
    identity_data.insert(
        format!("claim.{}.{}", claim.facet, claim.key),
        true
    );
}
if let Some(inst) = &ilk.institutional_role {
    identity_data.insert("institutional_role.office", inst.office);
    identity_data.insert("institutional_role.charter_ref", inst.charter_ref);
}
```

The matcher doesn't change. The map gets richer. Zero retrabajo in policy.

---

## Summary of implementation order

```
1. Freeze action_class canonical table + action_result contract (this note, point 2)
2. Router/admin/IO/WF inject action_class into event metadata
3. Emit action_result for evaluable action classes
4. Implement immutable system-axioms pack shipped with Fluxbee core
5. Install that pack into the `SY.architect` environment during system bootstrap
6. Implement composition into effective policy matrix
7. Implement axioms.rego generation from the installed system-axioms pack
8. Implement matrix-to-Rego compiler (solution rules only)
9. Implement jsr-policy SHM region for runtime exceptions / overrides
10. Implement local bootstrap persistence for active overrides
11. Implement SY.policy node (NATS consumer, post-facto evaluator)
12. Implement override writer + OPA reads overrides
13. Implement phased deploy (prepare → enable → validate → rollback)
14. Integrate into Blueprint reconciliation
```

Identity v3 is NOT in this sequence. Policy works against identity v2 as-is.
