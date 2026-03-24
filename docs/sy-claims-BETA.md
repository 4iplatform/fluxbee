# SY.claims — Normative Evaluation Engine (Beta)

**Status:** beta / direction
**Date:** 2026-03-14
**Audience:** Architecture team, OPA developers, cognitive developers, orchestrator
**Depends on:** `12-cognition-v2.md`, `identity-v3-direction.md`, OPA policies
**Relationship:** Consumes cognitive output, produces normative consequences for OPA and identity

---

## 1. Purpose

SY.claims is the normative evaluation layer of Fluxbee. It evaluates actions AFTER they occur and determines whether they violated system norms. It does not block actions in real time — it judges them post-facto and produces consequences that shape future enforcement.

**Analogy:**

| Layer | Role | Human equivalent | Speed |
|-------|------|-----------------|-------|
| Cognitive (reason signals) | 10 Commandments | Moral intuition, immediate | ~ms (per message) |
| OPA | Basic law enforcement | Police at the door | ~μs (per route decision) |
| SY.claims | Penal code + judiciary | Court system, evaluates after the fact | ~seconds (async, post-facto) |
| SY.architect | Executive override | Governor's pardon / legislative reform | Human-time |

---

## 2. Design Principles

### 2.1 Post-Facto, Not Pre-Facto

Claims does NOT sit in the message routing path. Messages flow through OPA and execute. Claims evaluates afterwards, in parallel, asynchronously. This keeps routing fast and the system responsive.

### 2.2 Actions First, Judgment Later

Like the human legal system: you act freely, but your actions have consequences. The system starts permissive and hardens where it detects problems. This is fundamentally different from traditional RBAC where everything is denied by default.

### 2.3 Consequences, Not Blocks

Claims does not block messages. It produces consequences: flags on ILKs, restrictions for future messages, alerts, escalations, or — critically — updates to OPA rules so that future similar actions ARE blocked preventively.

### 2.4 Reevaluable

Normative decisions are not permanent. They can be reevaluated based on time (penalty expires), reincidence (pattern confirms or contradicts), or executive override (architect pardons or reforms the rule).

---

## 3. Architecture

### 3.1 Position in the System

```
Message arrives
  │
  ▼
OPA (basic rules, fast, deterministic)
  │ allow/deny/route
  ▼
Execute (message delivered, action performed)
  │
  ├──► NATS (storage.turns) ──► SY.cognition (parallel, descriptive)
  │
  └──► NATS (storage.turns) ──► SY.claims (parallel, evaluative)
                                    │
                                    ├── No violation → nothing
                                    ├── Mild violation → flag + log
                                    ├── Serious violation → restrict ILK
                                    ├── Pattern detected → propose OPA rule update
                                    └── Critical → alert architect
```

### 3.2 Claims is a NATS Consumer

Like SY.cognition, SY.claims subscribes to `storage.turns`. It reads the same message stream but performs a different analysis: not "what is being discussed and with what drive" (cognitive) but "did this action comply with system norms" (normative).

### 3.3 Claims Consumes Cognitive Output

Claims does not re-extract tags or reason signals. It reads the cognitive enrichment that is already available:

- `memory_package` (contexts, reasons, memories, episodes) from jsr-memory SHM.
- Identity metadata from jsr-identity SHM.
- Action results from the message itself or from operation tracking.

This means claims depends on cognitive being operational, but does not interfere with it.

---

## 4. What Claims Evaluates

### 4.1 Per-Message Evaluation

For each message/action that flows through the system, claims asks:

- Did this action fit within the norms for this ILK's identity and claims?
- Did the reason signals indicate something that should be constrained?
- Does the pattern of behavior (from cognitive memory) suggest normative concern?
- Was there a previous restriction that was violated?

### 4.2 Inputs

```json
{
  "message_id": "msg:uuid",
  "thread_id": "thread:hash",
  "action_performed": "send_message | admin_command | config_change | ...",
  "identity": {
    "sender_ilk": "ilk:uuid",
    "ilk_type": "human | agent | system",
    "registration_status": "temporary | partial | complete",
    "tenant_id": "tnt:uuid",
    "active_restrictions": []
  },
  "cognitive_context": {
    "dominant_context": { "label": "billing dispute", "weight": 4.2 },
    "dominant_reason": { "label": "seeking urgent resolution", "weight": 3.8 },
    "reason_signals_canonical": ["resolve", "challenge"],
    "recent_episodes": [
      { "fear_id": "anger", "intensity": 8, "days_ago": 2 }
    ],
    "relevant_memories": [
      { "summary": "Juan brings billing issues as complaints...", "weight": 2.5 }
    ]
  },
  "action_result": {
    "status": "succeeded | failed | timeout",
    "target": "hive:worker-220",
    "side_effects": []
  }
}
```

### 4.3 What Claims Does NOT Evaluate

- Message content for moderation (that's a different concern, potentially a future AI.moderator).
- Routing correctness (that's OPA's job).
- Cognitive accuracy (that's SY.cognition's internal concern).

Claims evaluates normative compliance: did this actor, with this identity, performing this action, in this context, with this drive, stay within the rules?

---

## 5. Normative Rules

### 5.1 Two Tiers

**Tier 1 — Constitutional rules (system-wide, stable):**

Fundamental norms that apply to everyone. Rarely change. Examples:

- Temporary ILKs cannot perform system configuration actions.
- Unregistered entities cannot access tenant data outside their scope.
- Actions that modify system topology require a complete ILK.
- Repeated aggressive patterns (challenge + protect + abandon cycle) trigger review.

**Tier 2 — Charter rules (domain/office-specific, evolving):**

Norms that apply to specific roles, offices, or operational domains. Can be updated by architect. Examples:

- Billing support agents must not promise refunds above threshold X.
- Onboarding flows must complete identity verification before granting access.
- Government nodes must operate within their declared jurisdiction.

### 5.2 Rule Format (v1)

For v1, rules are declarative and simple:

```json
{
  "rule_id": "norm:001",
  "tier": "constitutional",
  "condition": {
    "ilk_registration_status": "temporary",
    "action_class": "system_config"
  },
  "consequence": "restrict",
  "severity": "serious",
  "description": "Temporary ILKs cannot modify system configuration"
}
```

```json
{
  "rule_id": "norm:012",
  "tier": "charter",
  "charter_ref": "charter://billing/support-v1",
  "condition": {
    "reason_signal_pattern": ["challenge", "abandon"],
    "pattern_count_threshold": 3,
    "pattern_window_days": 7
  },
  "consequence": "flag_and_alert",
  "severity": "mild",
  "description": "Repeated adversarial disengagement pattern in billing context"
}
```

### 5.3 Rule Storage

For v1: rules stored as JSON files in the SY.claims package (assets/rules/). Updated via `fluxbee-publish` like any package asset.

For future: rules managed by architect via ADMIN_COMMAND, stored in PostgreSQL, versioned.

---

## 6. Consequences

### 6.1 Consequence Types

| Type | Severity | Effect | Duration |
|------|----------|--------|----------|
| `log` | informational | Entry in claims audit log, no operational effect | Permanent record |
| `flag` | mild | Flag added to ILK metadata, visible to architect and AI nodes | Until cleared or expired |
| `restrict` | serious | Restriction added to ILK, OPA evaluates on next action | Until cleared, expired, or overridden |
| `alert` | serious | Notification sent to SY.architect | Until acknowledged |
| `propose_rule` | systemic | Proposed OPA rule update sent to architect for approval | Until approved or rejected |

### 6.2 Consequence Structure

```json
{
  "consequence_id": "csq:uuid",
  "rule_id": "norm:001",
  "trigger_message_id": "msg:uuid",
  "trigger_ilk": "ilk:uuid",
  "type": "restrict",
  "severity": "serious",
  "description": "Temporary ILK attempted system config action",
  "restriction": {
    "action_class": "system_config",
    "expires_at": null,
    "can_override": true
  },
  "created_at": "2026-03-14T10:00:00Z",
  "status": "active",
  "reviewed_by": null,
  "reviewed_at": null
}
```

### 6.3 How Restrictions Reach OPA

When claims produces a `restrict` consequence:

1. Claims writes the restriction to the ILK's metadata (via SY.identity or a dedicated claims store).
2. OPA reads `active_restrictions[]` from identity data on the next routing decision.
3. If the restriction matches the action being attempted, OPA denies it.

This creates the feedback loop: post-facto evaluation → restriction → preventive enforcement on next attempt.

### 6.4 Expiration and Decay

Consequences are not permanent by default:

- `flag`: expires after configurable TTL (default: 30 days without reincidence).
- `restrict`: expires after configurable TTL (default: 7 days) or when architect clears it.
- `alert`: resolved when architect acknowledges.
- `propose_rule`: resolved when architect approves or rejects.

If the same violation recurs before expiration, the TTL resets and severity may escalate.

---

## 7. Reincidence and Pattern Detection

### 7.1 Reincidence Tracking

Claims tracks violation patterns per ILK:

```json
{
  "ilk_id": "ilk:uuid",
  "violation_history": [
    {
      "rule_id": "norm:012",
      "count": 3,
      "first_at": "2026-03-10T10:00:00Z",
      "last_at": "2026-03-14T10:00:00Z",
      "consequences_applied": ["flag", "flag", "restrict"]
    }
  ]
}
```

### 7.2 Escalation by Reincidence

| Occurrence | Consequence |
|-----------|-------------|
| 1st | `log` or `flag` depending on severity |
| 2nd (same rule, within window) | `flag` escalates to `restrict` |
| 3rd+ | `restrict` + `alert` to architect |
| Persistent pattern | `propose_rule` for permanent OPA enforcement |

### 7.3 Reevaluation

Claims decisions can be reevaluated:

- **By time:** If no reincidence within the window, the flag/restriction expires naturally. The system forgives.
- **By architect override:** Architect can clear any consequence via ADMIN_COMMAND. This is the "governor's pardon."
- **By rule change:** If the architect modifies or removes a normative rule, existing consequences under that rule are automatically reevaluated.

---

## 8. Architect as Executive Authority

### 8.1 Override Powers

SY.architect can:

- Clear any flag or restriction on any ILK.
- Approve or reject proposed OPA rule updates.
- Modify normative rules (add, edit, disable).
- View full claims audit log.
- Override a specific evaluation ("this was not a violation").

### 8.2 Audit Trail

Every architect override is logged:

```json
{
  "override_id": "ovr:uuid",
  "consequence_id": "csq:uuid",
  "action": "clear_restriction",
  "reason": "Customer was justified in their complaint, agent was unresponsive",
  "overridden_by": "ilk:uuid-architect",
  "at": "2026-03-14T14:00:00Z"
}
```

---

## 9. Claims Evaluation Strategy

### 9.1 v1: Deterministic

For v1, claims evaluation is deterministic rule matching:

1. Read message + identity + cognitive context.
2. Match against normative rules (condition evaluation).
3. Check reincidence history.
4. Apply consequence based on severity + reincidence.

No AI involved. Rules are explicit. This is fast and auditable.

### 9.2 Future: Hybrid (Deterministic + AI)

When norms become complex enough that deterministic rules can't capture the nuance:

- Add AI-assisted evaluation for ambiguous cases.
- AI proposes; deterministic logic confirms or escalates.
- AI never directly applies consequences — always through the gate.

---

## 10. Relationship to Other Systems

### 10.1 Claims Does NOT Change Cognitive

Cognitive is closed. Claims consumes cognitive output (contexts, reasons, memories, episodes) as read-only input. Claims never writes to cognitive data structures or influences the cognitive pipeline.

### 10.2 Claims Does NOT Replace OPA

OPA remains the real-time enforcement engine. Claims feeds OPA with restrictions and proposed rules, but OPA makes its own decisions at routing time. Claims is the judiciary; OPA is the police.

### 10.3 Claims and Immediate Memory

When claims produces a flag or restriction, it can appear in the AI SDK's immediate memory as a `system_note`:

```json
{
  "role": "system",
  "kind": "system_note",
  "content": "NORMATIVE: ILK ilk:uuid-juan flagged for repeated adversarial pattern in billing context. Active restriction: system_config actions denied until 2026-03-21."
}
```

This way, AI nodes are aware of normative state when reasoning about their next response.

### 10.4 Claims and Identity

For v1, restrictions are stored in a claims-specific store (LanceDB local or PostgreSQL via SY.storage). OPA reads them via a dedicated claims data source, not from ILK metadata directly.

For future (identity v3): restrictions and normative claims could be part of the ILK's claims model (domain/function/responsibility + normative status).

---

## 11. Data Model

### 11.1 Normative Rule

```json
{
  "rule_id": "norm:uuid",
  "tier": "constitutional | charter",
  "charter_ref": "charter://...",
  "condition": {},
  "consequence_type": "log | flag | restrict | alert | propose_rule",
  "severity": "informational | mild | serious | critical",
  "description": "...",
  "enabled": true,
  "version": 1,
  "created_at": "...",
  "updated_at": "..."
}
```

### 11.2 Evaluation Record

```json
{
  "eval_id": "eval:uuid",
  "message_id": "msg:uuid",
  "thread_id": "thread:hash",
  "ilk_id": "ilk:uuid",
  "rules_matched": ["norm:001"],
  "rules_not_matched": [],
  "consequence_produced": "csq:uuid",
  "cognitive_snapshot": {
    "dominant_context_label": "billing dispute",
    "dominant_reason_label": "seeking urgent resolution",
    "reason_signals": ["resolve", "challenge"]
  },
  "evaluated_at": "..."
}
```

### 11.3 Consequence

```json
{
  "consequence_id": "csq:uuid",
  "rule_id": "norm:uuid",
  "eval_id": "eval:uuid",
  "ilk_id": "ilk:uuid",
  "type": "log | flag | restrict | alert | propose_rule",
  "severity": "...",
  "restriction": {},
  "status": "active | expired | cleared | overridden",
  "expires_at": "...",
  "created_at": "...",
  "cleared_by": null,
  "cleared_at": null,
  "override_reason": null
}
```

### 11.4 Reincidence Record

```json
{
  "ilk_id": "ilk:uuid",
  "rule_id": "norm:uuid",
  "occurrences": 3,
  "first_at": "...",
  "last_at": "...",
  "consequences": ["csq:uuid-1", "csq:uuid-2", "csq:uuid-3"],
  "escalation_level": 2
}
```

---

## 12. Storage

| Data | Location | Access |
|------|----------|--------|
| Normative rules | Package assets (v1) / PostgreSQL (future) | Read by SY.claims at startup |
| Evaluation records | PostgreSQL (via SY.storage) | Append-only, audit trail |
| Consequences (active) | LanceDB local + replicated via NATS events | Read by OPA for restrictions |
| Reincidence tracking | LanceDB local | Read/write by SY.claims |
| Architect overrides | PostgreSQL (via SY.storage) | Append-only, audit trail |

---

## 13. Implementation Tasks

### Phase 1 — Foundation

- [ ] CLM-T1. Create SY.claims node (Rust, NATS consumer).
- [ ] CLM-T2. Define normative rule schema and load from package assets.
- [ ] CLM-T3. Implement deterministic rule matcher (condition evaluation).
- [ ] CLM-T4. Implement consequence producer.
- [ ] CLM-T5. Store evaluation records and consequences in LanceDB.

### Phase 2 — OPA Integration

- [ ] CLM-T6. Implement restriction publishing (claims → NATS → OPA data source).
- [ ] CLM-T7. OPA reads active restrictions on routing decisions.
- [ ] CLM-T8. Implement expiration/decay of consequences.

### Phase 3 — Reincidence and Escalation

- [ ] CLM-T9. Implement reincidence tracking per ILK per rule.
- [ ] CLM-T10. Implement escalation logic (1st→flag, 2nd→restrict, 3rd→alert).
- [ ] CLM-T11. Implement reevaluation on rule change.

### Phase 4 — Architect Integration

- [ ] CLM-T12. ADMIN_COMMAND: `list_claims_consequences`, `clear_consequence`, `override_evaluation`.
- [ ] CLM-T13. ADMIN_COMMAND: `list_normative_rules`, `update_normative_rule`, `disable_normative_rule`.
- [ ] CLM-T14. ADMIN_COMMAND: `approve_proposed_rule`, `reject_proposed_rule`.
- [ ] CLM-T15. Claims audit log viewable from SY.architect.

### Phase 5 — E2E Validation

- [ ] CLM-T16. E2E: temporary ILK performs restricted action → claims flags → next attempt blocked by OPA.
- [ ] CLM-T17. E2E: reincidence escalation (3 violations → restrict + alert).
- [ ] CLM-T18. E2E: architect clears restriction → ILK can act again.
- [ ] CLM-T19. E2E: consequence expires by TTL → ILK unflagged automatically.
- [ ] CLM-T20. E2E: proposed OPA rule approved by architect → rule active on next evaluation.

---

## 14. What This Does NOT Cover (v1)

- Content moderation (hate speech, inappropriate content) — separate concern.
- Financial transaction enforcement — separate concern.
- AI-assisted normative evaluation — deferred to future hybrid mode.
- Cross-hive normative coordination — deferred.
- Full constitutional document management — deferred (see `identity-v3-direction.md`).

---

## 15. Summary

```
The 10 Commandments (cognitive reason signals)
  → Immediate moral intuition, per-message, cheap
  → "You should not steal"

The Penal Code (SY.claims normative rules)
  → Post-facto evaluation, pattern-aware, reevaluable
  → "Theft is punishable by restriction of access for 7 days"

The Police (OPA)
  → Real-time enforcement, deterministic, fast
  → "This ILK is currently restricted from system_config actions"

The Governor (SY.architect)
  → Executive override, human judgment, audit trail
  → "This restriction is cleared, the complaint was justified"
```

The system starts permissive. Actions happen freely under basic OPA rules. Claims evaluates post-facto and produces consequences that harden enforcement where needed. Architect can pardon, reform, or override at any time. The system learns its own law from experience.

---

## 16. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `12-cognition-v2.md` | Claims consumes cognitive output (contexts, reasons, memories, episodes). Does not modify cognitive |
| `identity-v3-direction.md` | Future: claims/restrictions may integrate with ILK claims model |
| `admin-internal-gateway-spec.md` | Architect manages claims via ADMIN_COMMAND |
| `immediate-conversation-memory-spec.md` | Normative flags appear as system_notes in immediate memory |
| `sy-architect-spec.md` | Architect is the executive authority over claims |
| OPA policies | Claims feeds restrictions to OPA. OPA enforces. Claims proposes rule updates |
