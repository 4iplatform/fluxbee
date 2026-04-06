# SY.policy — Normative Evaluation Engine (Beta)

**Status:** beta
**Date:** 2026-03-31
**Audience:** Architecture team, OPA developers, identity developers, orchestrator
**Depends on:** `12-cognition-v2.md`, `10-identity-v2.md`, OPA, jsr-identity SHM
**Replaces:** `sy-claims-beta.md` (renamed from SY.claims to SY.policy)

> Compatibility note (2026-04-05): `SY.policy` v1 runs on current Identity v2 flat metadata (`registration_status`, `tenant_id`, `ilk_type`, `roles[]`, `capabilities[]`). Identity v3 structured claims remain a later extension. New implementation should be forward-compatible with that extension, but must not depend on the Identity v3 migration now.

---

## 1. Purpose

SY.policy is the normative evaluation engine of Fluxbee. It does three things:

1. **Defines the law** — a policy matrix of claims × actions × effects that the Blueprint declares and OPA executes.
2. **Watches compliance** — evaluates actions post-facto against the law and the ILK's claims.
3. **Produces overrides** — when a violation is detected, writes temporary overrides that OPA applies immediately.

SY.policy does NOT route messages, does NOT block actions in real-time, and does NOT replace OPA. It feeds OPA with data.

---

## 2. The Three Columns

The entire normative system rests on three concepts:

| Column | Source | Question |
|--------|--------|----------|
| **Claim** | Identity (SY.identity) | Who are you? What do you declare? |
| **Action** | Message stream (NATS) | What did you do? |
| **Law** | Blueprint (policy_matrix) | What is allowed given your claims and your action? |

SY.policy compares: did the action that occurred fit with the claims of the ILK according to the laws in effect? If not → override.

---

## 3. Policy Matrix (The Law)

### 3.1 Structure

The policy matrix is a table of rules. Each rule says: "if an ILK with these claims performs this action, the effect is X."

```json
{
  "policy_matrix": [
    {
      "rule_id": "pm:001",
      "claim": { "registration_status": "temporary" },
      "action_class": "send_message",
      "effect": "route_to_frontdesk",
      "tier": "constitutional",
      "description": "Temporary ILKs go to frontdesk for registration"
    },
    {
      "rule_id": "pm:002",
      "claim": { "registration_status": "temporary" },
      "action_class": "system_config",
      "effect": "deny",
      "tier": "constitutional",
      "description": "Temporary ILKs cannot modify system configuration"
    },
    {
      "rule_id": "pm:003",
      "claim": { "capability": "billing" },
      "action_class": "external_action",
      "effect": "allow",
      "tier": "charter",
      "charter_ref": "charter://acme/support-v1",
      "description": "Billing agents can perform external actions in billing domain"
    },
    {
      "rule_id": "pm:004",
      "claim": { "role": "support_l1" },
      "action_class": "topology_change",
      "effect": "deny",
      "tier": "constitutional",
      "description": "L1 support agents cannot change system topology"
    },
    {
      "rule_id": "pm:005",
      "claim": { "registration_status": "complete" },
      "action_class": "workflow_step",
      "effect": "allow",
      "tier": "constitutional",
      "description": "Complete ILKs can participate in workflows"
    }
  ]
}
```

### 3.2 Claim Matching

The `claim` field matches against identity data from jsr-identity SHM:

| Claim field | Identity source |
|------------|----------------|
| `registration_status` | ILK.registration_status |
| `capability` | ILK.definition.current.capabilities[] |
| `role` | ILK.definition.current.roles[] |
| `tenant_id` | ILK.association.tenant_id |
| `ilk_type` | ILK.ilk_type (human/agent/system) |

For v1, claims are the flat roles/capabilities from identity v2. When identity v3 adds structured claims (domain/function/responsibility), the matching extends naturally.

### 3.3 Action Classes

A fixed vocabulary of what can happen in the system:

| Action class | Meaning | Examples |
|-------------|---------|---------|
| `send_message` | Regular message between nodes/users | Chat, notification |
| `read` | Read system state or data | GET inventory, GET config |
| `write` | Modify data within existing scope | Update config, update tenant |
| `system_config` | Modify system configuration | Set routes, set OPA, set storage config |
| `topology_change` | Modify system structure | Add/remove hive, add/remove VPN |
| `external_action` | Action that touches the outside world | Send WhatsApp, call external API |
| `identity_change` | Modify identity data | Register ILK, update capabilities |
| `workflow_step` | Execute a step in a workflow | Trigger, advance, complete |
| `node_lifecycle` | Spawn, kill, or reconfigure a node | run_node, kill_node, set_node_config |

### 3.4 Effects

What happens when a claim + action matches a rule:

| Effect | Meaning |
|--------|---------|
| `allow` | Action is permitted |
| `deny` | Action is blocked |
| `route_to_frontdesk` | Message is redirected to AI.frontdesk |
| `route_to` | Message is redirected to a specific node |
| `require_confirmation` | Action requires explicit confirmation before executing |
| `flag` | Action is allowed but logged for review |

### 3.5 Rule Tiers

| Tier | Source | Mutability |
|------|--------|-----------|
| `constitutional` | Fluxbee system defaults + Blueprint | Rarely changes. Architect can modify but needs good reason |
| `charter` | Solution-specific Blueprint section | Changes with solution evolution. Charter_ref links to the source |

### 3.6 Default Effect

If no rule matches a claim + action combination: **allow**. The system starts permissive. SY.policy hardens it over time when violations are detected.

---

## 4. OPA Execution

### 4.1 How the Matrix Becomes OPA

The policy matrix compiles to Rego deterministically. Each rule becomes a Rego clause:

```rego
# From pm:001
route_to_frontdesk {
    input.identity.registration_status == "temporary"
    input.action_class == "send_message"
}

# From pm:002
deny {
    input.identity.registration_status == "temporary"
    input.action_class == "system_config"
}

# From pm:004
deny {
    input.identity.roles[_] == "support_l1"
    input.action_class == "topology_change"
}
```

The Blueprint generates this Rego. The architect applies it via `opa_apply`. It lives in jsr-opa SHM as compiled WASM.

### 4.2 Policy Overrides in OPA

In addition to the base rules, OPA reads a table of overrides from SHM:

```rego
# Override: deny wins over any base allow
deny {
    override := data.policy_overrides[_]
    override.ilk_id == input.identity.ilk_id
    override.action_class == input.action_class
    override.effect == "deny"
    time.now_ns() < override.expires_at_ns
}

# Override: flag (allow but mark)
flag {
    override := data.policy_overrides[_]
    override.ilk_id == input.identity.ilk_id
    override.action_class == input.action_class
    override.effect == "flag"
    time.now_ns() < override.expires_at_ns
}
```

Override always wins over base rule. This is how SY.policy affects OPA without reprogramming it.

### 4.3 What OPA Reads

OPA reads from two SHM sources, both already available at routing time:

| Source | Data | Speed |
|--------|------|-------|
| jsr-identity SHM | ILK claims: registration_status, roles, capabilities, tenant_id, ilk_type | ~μs |
| jsr-opa SHM | Base Rego rules (compiled WASM) | ~μs |
| jsr-policy SHM (new) | policy_overrides[] table | ~μs |

Total routing decision: identity lookup + base rule eval + override check. All SHM, all μs.

---

## 5. SY.policy Post-Facto Evaluation

### 5.1 Position in the System

```
Message arrives
  → Router reads identity (jsr-identity SHM)
  → Router reads base rules (jsr-opa SHM) + overrides (jsr-policy SHM)
  → OPA decides: allow/deny/route (μs)
  → Message delivered or blocked
  → Router publishes to NATS (storage.turns)

In parallel (async, post-facto):
  → SY.policy reads from NATS
  → Builds normative case: who acted + what they did + what claims they have
  → Compares against policy_matrix
  → If violation detected: writes override to jsr-policy SHM
  → Override effective on next OPA evaluation for that ILK
```

### 5.2 What SY.policy Evaluates

For each action in the message stream, policy asks:

1. What claims does this ILK have? (read from identity)
2. What action was performed? (read from message)
3. What does the policy_matrix say for this claim + action? (read from loaded rules)
4. Did the action match the expected effect? (compare)
5. Are there existing overrides that should have prevented this? (check)
6. Is there a reincidence pattern? (check history)

### 5.3 Building the Normative Case

```json
{
  "case_id": "case:uuid",
  "message_id": "msg:uuid",
  "trace_id": "trace:uuid",
  "thread_id": "thread:hash",
  "tenant_id": "tnt:uuid",

  "subjects": {
    "actor_ilk": "ilk:uuid",
    "ilk_type": "human",
    "registration_status": "complete",
    "roles": ["support_l1"],
    "capabilities": ["billing"],
    "active_overrides": []
  },

  "act": {
    "action_performed": "admin_command",
    "action_class": "system_config",
    "target_ref": "route:billing.refunds",
    "result_status": "succeeded"
  },

  "evaluation": {
    "matched_rules": ["pm:004"],
    "expected_effect": "deny",
    "actual_outcome": "succeeded",
    "violation": true,
    "degraded_mode": false
  }
}
```

### 5.4 Degraded Mode

If identity data is unavailable, policy evaluates with what it has:

- **Normal mode:** Full claim + action + law evaluation.
- **Degraded mode:** Action + result only. Constitutional rules that don't depend on claims still fire (e.g., "no action_class=topology_change from any ILK during maintenance window").

A cognitive failure does NOT affect policy. Policy does not depend on cognitive data in v1.

---

## 6. Overrides

### 6.1 Override Structure

When policy detects a violation, it writes an override:

```json
{
  "override_id": "ovr:uuid",
  "ilk_id": "ilk:uuid",
  "action_class": "system_config",
  "effect": "deny",
  "expires_at": "2026-03-31T00:00:00Z",
  "source_case_id": "case:uuid",
  "source_rule_id": "pm:004",
  "severity": "serious",
  "reincidence_level": 1,
  "created_at": "2026-03-24T18:10:00Z"
}
```

### 6.2 Override Effects

| Effect | OPA behavior |
|--------|-------------|
| `deny` | Block the action |
| `flag` | Allow but mark in audit log |
| `require_confirmation` | Force confirmation flow before execution |
| `route_to` | Redirect to a specific node (e.g., supervisor) |

### 6.3 Override Lifecycle

```
Violation detected → Override written → OPA reads it → Effect active
  │
  ├── TTL expires → Override removed → Effect lifted
  ├── Reincidence → TTL resets, severity may escalate
  ├── Architect clears → Override removed → Effect lifted
  └── Rule changed → Overrides under that rule reevaluated
```

### 6.4 Expiration and Escalation

| Severity | Default TTL | Escalation |
|----------|------------|-----------|
| `informational` | 7 days | No escalation |
| `mild` | 14 days | 2nd violation: escalate to serious |
| `serious` | 30 days | 3rd violation: escalate to critical |
| `critical` | No expiration | Requires architect to clear |

Reincidence resets the TTL and may escalate severity.

### 6.5 SHM: jsr-policy-\<hive\>

New SHM region. Single writer: SY.policy. Uses seqlock (same as all other regions).

Contains the `policy_overrides[]` table. OPA reads it at routing time alongside jsr-identity and jsr-opa.

Layout: array of fixed-size override entries. Max entries configurable (default: 1000 — overrides should be few; if you have thousands, the base rules need fixing).

---

## 7. Reincidence Tracking

### 7.1 Per ILK Per Rule

```json
{
  "ilk_id": "ilk:uuid",
  "rule_id": "pm:004",
  "violation_count": 3,
  "first_at": "2026-03-10T10:00:00Z",
  "last_at": "2026-03-24T18:10:00Z",
  "override_ids": ["ovr:uuid-1", "ovr:uuid-2", "ovr:uuid-3"],
  "current_severity": "serious"
}
```

### 7.2 Escalation Logic

| Occurrence | Action |
|-----------|--------|
| 1st | Override with base severity from rule |
| 2nd (same rule, within window) | TTL reset + severity escalation |
| 3rd+ | Escalation + alert to architect |
| Sustained pattern (5+) | Critical override (no expiration) |

### 7.3 Forgiveness

If no violation for a configurable window (default: 2× TTL), reincidence counter resets. The system forgives.

---

## 8. Architect as Governor

### 8.1 Powers

The architect can, via ADMIN_COMMAND:

- `list_policy_overrides` — view all active overrides.
- `clear_override` — remove a specific override. The ILK can act again.
- `list_policy_matrix` — view the current law (all rules).
- `update_policy_matrix` — modify rules (add, edit, disable). Triggers Rego recompilation.
- `list_violations` — view evaluation history and cases.

### 8.2 Every Override is Clearable

Every override can be cleared by the architect at any time. This is the "governor's pardon." The clearing is logged with reason and ILK of the architect.

### 8.3 Audit Trail

All evaluations, overrides, clearings, and rule changes are logged:

```json
{
  "event_type": "override_cleared",
  "override_id": "ovr:uuid",
  "cleared_by": "ilk:uuid-architect",
  "reason": "Customer complaint was justified, agent was unresponsive",
  "at": "2026-03-24T20:00:00Z"
}
```

---

## 9. Relationship to Blueprint

### 9.1 Blueprint Declares the Law

The Hive Blueprint `opa` section contains the `policy_matrix`. When the blueprint is applied, the matrix compiles to Rego and loads into jsr-opa SHM.

### 9.2 Blueprint Also Declares Normative Rules

The Blueprint `normative` section declares two-tier rules (constitutional + charter) that SY.policy uses for post-facto evaluation. These are the same rules — the policy_matrix serves both OPA (real-time) and SY.policy (post-facto).

### 9.3 Single Source of Truth

```
Blueprint.policy_matrix
  → compiles to Rego → jsr-opa SHM (OPA real-time enforcement)
  → loaded by SY.policy (post-facto evaluation reference)
  
SY.policy evaluations
  → produces overrides → jsr-policy SHM (OPA reads at routing time)
```

One matrix, two consumers, two purposes.

---

## 10. Data Flow Summary

```
DESIGN TIME (Blueprint)
  Architect declares policy_matrix (claims × actions → effects)
  Architect applies → compiles to Rego → jsr-opa SHM

ROUTING TIME (μs)
  Router reads: jsr-identity (claims) + jsr-opa (base rules) + jsr-policy (overrides)
  OPA evaluates: claims + action + base_rules + overrides → decision
  Override wins over base rule

POST-FACTO (async, seconds)
  SY.policy reads NATS stream
  Builds case: who + what + claims + law
  If violation: writes override to jsr-policy SHM
  If reincidence: escalates severity
  If pattern: alert to architect
  
GOVERNANCE (human time)
  Architect reviews overrides
  Architect clears overrides when appropriate
  Architect modifies policy_matrix when law needs to change
```

---

## 11. Storage

| Data | Location | Writer |
|------|----------|--------|
| Policy matrix (rules) | Blueprint package assets + jsr-opa SHM (compiled) | Architect (via opa_apply) |
| Policy overrides (active) | jsr-policy SHM | SY.policy |
| Evaluation records (cases) | LanceDB local + PostgreSQL via SY.storage | SY.policy |
| Reincidence tracking | LanceDB local | SY.policy |
| Audit log (overrides, clearings) | PostgreSQL via SY.storage | SY.policy, SY.architect |

---

## 12. Implementation Tasks

### Phase 1 — Policy Matrix and OPA Compilation

- [ ] POL-T1. Define policy_matrix JSON schema (this document).
- [ ] POL-T2. Implement matrix-to-Rego compiler (deterministic).
- [ ] POL-T3. Integrate compiled matrix into existing `opa_apply` flow.
- [ ] POL-T4. Verify OPA reads base rules from jsr-opa SHM correctly.

### Phase 2 — Override Mechanism

- [ ] POL-T5. Create jsr-policy SHM region for policy_overrides[].
- [ ] POL-T6. OPA reads overrides from jsr-policy SHM alongside base rules.
- [ ] POL-T7. Override wins over base rule (deny override > allow base).
- [ ] POL-T8. Override expiration by TTL (time-based check in OPA).

### Phase 3 — SY.policy Node

- [ ] POL-T9. Create SY.policy node (Rust, NATS consumer on storage.turns).
- [ ] POL-T10. Implement normative case builder (subjects + act from message + identity).
- [ ] POL-T11. Implement policy_matrix evaluation (claim + action → expected effect vs actual).
- [ ] POL-T12. Implement override writer to jsr-policy SHM.

### Phase 4 — Reincidence and Escalation

- [ ] POL-T13. Implement reincidence tracking per ILK per rule.
- [ ] POL-T14. Implement severity escalation logic.
- [ ] POL-T15. Implement forgiveness (counter reset after 2× TTL without violation).

### Phase 5 — Architect Integration

- [ ] POL-T16. ADMIN_COMMAND: `list_policy_overrides`, `clear_override`.
- [ ] POL-T17. ADMIN_COMMAND: `list_policy_matrix`, `update_policy_matrix`.
- [ ] POL-T18. ADMIN_COMMAND: `list_violations`.
- [ ] POL-T19. Audit logging for all policy events.

### Phase 6 — E2E Validation

- [ ] POL-T20. E2E: temporary ILK + system_config → deny by base rule.
- [ ] POL-T21. E2E: complete ILK violates charter rule → policy override written → next attempt denied.
- [ ] POL-T22. E2E: reincidence escalation (3 violations → serious override).
- [ ] POL-T23. E2E: architect clears override → ILK can act again.
- [ ] POL-T24. E2E: override expires by TTL → ILK unflagged automatically.
- [ ] POL-T25. E2E: Blueprint change → new matrix compiled → OPA updated.

---

## 13. What This Does NOT Cover (v1)

- Content moderation (separate concern).
- AI-assisted normative evaluation (v1 is deterministic only).
- Cross-hive normative coordination.
- Complex delegation chains (principal_ilk tracing).
- Cognitive data as evidence for policy decisions (future additive).

---

## 14. Summary

```
CLAIMS (Identity)     ×     ACTIONS (Message)     →     LAWS (Matrix)
  Who are you?                What did you do?           What should happen?

Blueprint declares the matrix.
OPA executes it in μs.
SY.policy watches post-facto and writes overrides.
Overrides win over base rules.
Architect clears overrides when needed.
The system starts permissive and hardens where it detects violations.
```

---

## 15. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `10-identity-v2.md` | Claims come from ILK metadata (roles, capabilities, registration_status) |
| `12-cognition-v2.md` | Cognitive is separate. Policy does not depend on cognitive in v1 |
| `identity-v3-direction.md` | Future: structured claims (domain/function/responsibility) extend claim matching |
| `sy-architect-spec.md` | Architect is governor: clears overrides, modifies matrix |
| `solution-manifest-spec.md` (Hive Blueprint) | Blueprint declares the policy_matrix |
| OPA / jsr-opa SHM | Base rules compiled from matrix |
| jsr-policy SHM (new) | Override table written by SY.policy, read by OPA |
| jsr-identity SHM | ILK claims read by OPA at routing time |
