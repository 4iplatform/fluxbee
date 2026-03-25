# SY.claims — Normative Evaluation Engine (Beta)

**Status:** beta / direction
**Date:** 2026-03-24
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

## 4. The Normative Case

### 4.1 Three Distinct Objects

Claims operates on three distinct objects, kept separate:

| Object | Purpose | Contains |
|--------|---------|----------|
| **Normative Case** | The file of the incident | Who acted, what they did, what resulted, what cognitive evidence exists |
| **Evaluation Record** | The judgment | Which rules matched, what claims were derived, provenance for reproducibility |
| **Consequence** | The sanction | Flag, restriction, alert, proposed rule. With expiration and override |

### 4.2 Normative Case Structure

The normative case is the canonical representation of "what happened." It separates hard facts from cognitive evidence.

```json
{
  "case_id": "case:uuid",
  "message_id": "msg:uuid",
  "trace_id": "trace:uuid",
  "thread_id": "thread:hash",
  "scope_id": "scope:uuid|null",
  "tenant_id": "tnt:uuid",
  "opened_at": "2026-03-24T18:10:00Z",

  "subjects": {
    "actor_ilk": "ilk:uuid",
    "principal_ilk": "ilk:uuid|null",
    "executor_node": "AI.support.l1@prod|null",
    "affected_ilks": ["ilk:uuid-1"],
    "sender_type": "human|agent|system",
    "registration_status": "temporary|partial|complete",
    "active_restrictions": []
  },

  "act": {
    "action_performed": "send_message|admin_command|config_change|tool_call|workflow_step",
    "action_class": "message|read|write|system_config|topology_change|external_action|identity_change",
    "target_class": "ilk|thread|scope|route_table|vpn_rule|node|workflow|external_api|tenant_data",
    "target_ref": "route:billing.refunds|null",
    "effect_class": "none|read|write|execute|restrict|notify",
    "authority_source": "role|capability|explicit_override|system_default|unknown",
    "touches_system_config": false,
    "touches_external_world": false,
    "touches_tenant_boundary": false
  },

  "result": {
    "status": "succeeded|failed|timeout|rejected",
    "side_effects": [
      { "kind": "route_updated|message_sent|restriction_violated", "ref": "..." }
    ],
    "restriction_violated": false
  },

  "cognitive_evidence": {
    "dominant_context": {
      "context_id": "context:uuid",
      "label": "billing dispute",
      "weight": 4.2
    },
    "dominant_reason": {
      "reason_id": "reason:uuid",
      "label": "seeking urgent resolution",
      "weight": 3.8
    },
    "reason_signals_canonical": ["resolve", "challenge"],
    "reason_signals_extra": ["urgency", "frustration"],
    "memory_refs": [
      { "memory_id": "memory:uuid", "weight": 2.5 }
    ],
    "episode_refs": [
      { "episode_id": "episode:uuid", "intensity": 8, "days_ago": 2 }
    ]
  },

  "normative_claims": {
    "intent_class": "request_change|request_read|inform_only|challenge_action|confirm_action|withdrawal",
    "risk_level": "low|medium|high|critical",
    "requires_confirmation": false,
    "human_handoff_candidate": false,
    "read_only": false,
    "write_candidate": true,
    "needs_review": false,
    "policy_domain": "billing|identity|routing|topology|general",
    "prior_pattern_match": false
  }
}
```

### 4.3 Key Separations

**Subjects:** Distinguishes who acted (`actor_ilk`), on whose behalf (`principal_ilk`), which node executed (`executor_node`), and who was affected (`affected_ilks`). This prevents blindly punishing the executor when the intent came from elsewhere. For v1, `principal_ilk` and `affected_ilks` are optional.

**Act:** The core of the judgment. `action_class` + `target_class` + `effect_class` + `authority_source` give claims and OPA a stable interface that doesn't depend on narrative or cognitive labels. This is what rules match against.

**Cognitive evidence:** Input from SY.cognition, used as supporting evidence. NOT the primary basis for judgment on serious consequences. The primary basis is `act` + `result` + `subjects`. Cognitive evidence enriches the judgment but does not define it.

**Normative claims:** Derived structured facts that bridge cognitive evidence to OPA-consumable decisions. These are canonical, few, and hard — not narrative.

### 4.4 What Claims Does NOT Evaluate

- Message content for moderation (separate concern, future AI.moderator).
- Routing correctness (OPA's job).
- Cognitive accuracy (SY.cognition's internal concern).
- Re-extraction of semantic tags or reason signals (uses cognitive output as-is).

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

Rules match against the normative case's `subjects`, `act`, `result`, and optionally `normative_claims`. They do NOT match against cognitive evidence narrative or labels directly.

**Constitutional rule (matches act + subjects):**

```json
{
  "rule_id": "norm:001",
  "tier": "constitutional",
  "version": 1,
  "condition": {
    "subjects.registration_status": "temporary",
    "act.action_class": "system_config"
  },
  "consequence_type": "restrict",
  "restriction_spec": {
    "deny_action_class": "system_config",
    "ttl_days": null
  },
  "severity": "serious",
  "description": "Temporary ILKs cannot modify system configuration"
}
```

**Charter rule (matches act + pattern from reincidence):**

```json
{
  "rule_id": "norm:012",
  "tier": "charter",
  "version": 1,
  "charter_ref": "charter://billing/support-v1",
  "condition": {
    "act.action_class": "message",
    "normative_claims.policy_domain": "billing",
    "reincidence.rule_id": "norm:012",
    "reincidence.count_gte": 3,
    "reincidence.window_days": 7
  },
  "consequence_type": "flag_and_alert",
  "severity": "mild",
  "description": "Repeated adversarial disengagement pattern in billing context"
}
```

**Key rule:** conditions reference normative case fields using dot notation. Cognitive evidence labels like "billing dispute" are NOT used as match conditions — they inform `normative_claims.policy_domain` which IS matchable.
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

When claims produces a `restrict` consequence, it emits a restriction in a format that OPA can consume directly — concrete, small, and unambiguous:

```json
{
  "restriction_id": "rst:uuid",
  "ilk_id": "ilk:uuid",
  "deny_action_class": "system_config",
  "deny_target_class": null,
  "expires_at": "2026-03-31T00:00:00Z",
  "source_consequence_id": "csq:uuid"
}
```

**OPA restriction format rules:**

- Restrictions are tiny and concrete: `deny action_class=X until Y`.
- No narrative, no labels, no cognitive content reaches OPA as restriction.
- OPA reads `active_restrictions[]` from claims store and matches against the action being attempted.
- If match: deny. If no match: allow (OPA's other rules still apply).

**Examples of valid restrictions:**

```
deny action_class=system_config until 2026-03-31
deny action_class=topology_change until 2026-03-21
require_human_handoff for policy_domain=billing until 2026-03-28
read_only for target_class=tenant_data until 2026-04-01
```

**Examples of INVALID restrictions (too narrative, too vague):**

```
"repeated adversarial pattern in billing context"  ← NOT a restriction
"user seems frustrated"                             ← NOT a restriction
"high risk level detected"                          ← NOT a restriction
```

The motivo/narrative goes in the evaluation record and consequence audit trail. OPA never sees it.

This creates the feedback loop: post-facto evaluation → concrete restriction → preventive enforcement on next attempt.

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

1. Build normative case from message + identity + action + result + cognitive evidence.
2. Derive normative_claims (intent_class, risk_level, etc.) deterministically from case facts.
3. Match normative case against rules (conditions reference case fields via dot notation).
4. Check reincidence history for escalation.
5. Produce consequence based on severity + reincidence.

No AI involved. Rules are explicit. This is fast and auditable.

### 9.2 Evaluation Provenance

Every evaluation record carries provenance for reproducibility and appeal:

```json
{
  "eval_id": "eval:uuid",
  "case_id": "case:uuid",
  "degraded_mode": false,
  "claims_engine_version": "1.0.0",
  "rule_set_version": "claims-rules@2026-03-24",
  "cognitive_snapshot_ref": "cogsnap:uuid|null",
  "rules_matched": ["norm:001"],
  "rules_not_matched": ["norm:012"],
  "basis": [
    "subjects.registration_status=temporary",
    "act.action_class=system_config"
  ],
  "consequence_id": "csq:uuid|null",
  "evaluated_at": "2026-03-24T18:10:02Z"
}
```

`basis` lists the specific field values that caused each rule match. This makes the judgment reproducible: if rules or engine version change, reevaluation can be traced.

### 9.3 Degraded Mode

If SY.cognition is down, delayed, or has not yet processed the message, claims operates in degraded mode:

**With cognitive enrichment (normal):** Full evaluation using all normative case fields including `cognitive_evidence` and derived `normative_claims`.

**Without cognitive enrichment (degraded):** Claims evaluates using only `subjects` + `act` + `result`. The `cognitive_evidence` block is null. `normative_claims` are derived from act/subjects only (no intent_class from reason signals, no policy_domain from context). The evaluation record marks `degraded_mode: true`.

**Rule:** A cognitive failure must never relax the normative dimension. Constitutional rules (which match on subjects + act) still fire in degraded mode. Only charter rules that depend on cognitive evidence are skipped.

### 9.4 Evidence Weight for Serious Consequences

For mild consequences (`log`, `flag`): cognitive evidence + act facts are sufficient.

For serious consequences (`restrict`, `alert`, `propose_rule`): the primary basis must be `act` + `result` + `subjects`. Cognitive evidence is supporting, not primary. This prevents a narrative label from causing a hard restriction.

| Consequence | Primary basis | Supporting |
|------------|--------------|------------|
| `log` | Any matching field | Any |
| `flag` | act + subjects OR cognitive pattern | Any |
| `restrict` | act + subjects + result | Cognitive evidence |
| `alert` | act + subjects + result | Cognitive evidence |
| `propose_rule` | Sustained pattern in act + reincidence | Cognitive evidence |

### 9.5 Future: Hybrid (Deterministic + AI)

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
  "charter_ref": "charter://...|null",
  "version": 1,
  "condition": {
    "subjects.registration_status": "temporary",
    "act.action_class": "system_config"
  },
  "consequence_type": "log | flag | restrict | alert | propose_rule",
  "restriction_spec": {
    "deny_action_class": "system_config",
    "deny_target_class": null,
    "ttl_days": 7
  },
  "severity": "informational | mild | serious | critical",
  "requires_cognitive": false,
  "description": "...",
  "enabled": true,
  "created_at": "...",
  "updated_at": "..."
}
```

`requires_cognitive: false` means the rule fires in degraded mode (no cognitive data). Constitutional rules should generally be `false`. Charter rules that depend on cognitive evidence should be `true`.

### 11.2 Normative Case

See section 4.2 for the full structure. This is the "file" — the canonical representation of what happened, with subjects, act, result, cognitive_evidence, and normative_claims.

### 11.3 Evaluation Record

```json
{
  "eval_id": "eval:uuid",
  "case_id": "case:uuid",
  "degraded_mode": false,
  "claims_engine_version": "1.0.0",
  "rule_set_version": "claims-rules@2026-03-24",
  "cognitive_snapshot_ref": "cogsnap:uuid|null",
  "rules_matched": ["norm:001"],
  "rules_not_matched": ["norm:012"],
  "basis": [
    "subjects.registration_status=temporary",
    "act.action_class=system_config"
  ],
  "consequence_id": "csq:uuid|null",
  "evaluated_at": "..."
}
```

`basis` makes the judgment reproducible. `cognitive_snapshot_ref` traces what cognitive state existed at evaluation time.

### 11.4 Consequence

```json
{
  "consequence_id": "csq:uuid",
  "rule_id": "norm:uuid",
  "eval_id": "eval:uuid",
  "ilk_id": "ilk:uuid",
  "type": "log | flag | restrict | alert | propose_rule",
  "severity": "...",
  "restriction": {
    "restriction_id": "rst:uuid",
    "deny_action_class": "system_config",
    "deny_target_class": null,
    "expires_at": "2026-03-31T00:00:00Z"
  },
  "status": "active | expired | cleared | overridden",
  "created_at": "...",
  "cleared_by": null,
  "cleared_at": null,
  "override_reason": null
}
```

### 11.5 Reincidence Record

```json
{
  "ilk_id": "ilk:uuid",
  "rule_id": "norm:uuid",
  "occurrences": 3,
  "first_at": "...",
  "last_at": "...",
  "consequence_ids": ["csq:uuid-1", "csq:uuid-2", "csq:uuid-3"],
  "escalation_level": 2
}
```

---

## 12. Storage

| Data | Location | Access |
|------|----------|--------|
| Normative rules | Package assets (v1) / PostgreSQL (future) | Read by SY.claims at startup |
| Normative cases | LanceDB local (hot) + PostgreSQL via SY.storage (durable) | Written per evaluation |
| Evaluation records | PostgreSQL (via SY.storage) | Append-only, audit trail |
| Consequences (active) | LanceDB local + replicated via NATS events | Read by OPA for restrictions |
| Reincidence tracking | LanceDB local | Read/write by SY.claims |
| Architect overrides | PostgreSQL (via SY.storage) | Append-only, audit trail |

---

## 13. Implementation Tasks

### Phase 1 — Foundation

- [ ] CLM-T1. Create SY.claims node (Rust, NATS consumer on `storage.turns`).
- [ ] CLM-T2. Define normative rule schema with dot-notation conditions. Load from package assets.
- [ ] CLM-T3. Implement normative case builder (subjects + act + result + cognitive_evidence + normative_claims).
- [ ] CLM-T4. Implement deterministic rule matcher (conditions match against case fields).
- [ ] CLM-T5. Implement normative_claims derivation from case facts (intent_class, risk_level, etc.).
- [ ] CLM-T6. Implement consequence producer with restriction_spec for OPA.
- [ ] CLM-T7. Store normative cases, evaluation records, and consequences in LanceDB.

### Phase 2 — Degraded Mode and Evidence Weight

- [ ] CLM-T8. Implement degraded mode: evaluate with subjects + act + result only when cognitive data unavailable.
- [ ] CLM-T9. Mark rules with `requires_cognitive` flag. Skip cognitive-dependent rules in degraded mode.
- [ ] CLM-T10. Implement evidence weight rules: restrict/alert require act+subjects+result as primary basis.

### Phase 3 — OPA Integration

- [ ] CLM-T11. Implement restriction publishing (concrete deny_action_class format, via NATS).
- [ ] CLM-T12. OPA reads active restrictions on routing decisions.
- [ ] CLM-T13. Implement expiration/decay of consequences.

### Phase 4 — Reincidence and Escalation

- [ ] CLM-T14. Implement reincidence tracking per ILK per rule.
- [ ] CLM-T15. Implement escalation logic (1st→flag, 2nd→restrict, 3rd→alert).
- [ ] CLM-T16. Implement reevaluation on rule change.

### Phase 5 — Architect Integration

- [ ] CLM-T17. ADMIN_COMMAND: `list_claims_consequences`, `clear_consequence`, `override_evaluation`.
- [ ] CLM-T18. ADMIN_COMMAND: `list_normative_rules`, `update_normative_rule`, `disable_normative_rule`.
- [ ] CLM-T19. ADMIN_COMMAND: `approve_proposed_rule`, `reject_proposed_rule`.
- [ ] CLM-T20. Claims audit log viewable from SY.architect (case + evaluation + consequence chain).

### Phase 6 — E2E Validation

- [ ] CLM-T21. E2E: temporary ILK performs restricted action → claims builds case → flags → next attempt blocked by OPA.
- [ ] CLM-T22. E2E: reincidence escalation (3 violations → restrict + alert).
- [ ] CLM-T23. E2E: architect clears restriction → ILK can act again.
- [ ] CLM-T24. E2E: consequence expires by TTL → ILK unflagged automatically.
- [ ] CLM-T25. E2E: proposed OPA rule approved by architect → rule active on next evaluation.
- [ ] CLM-T26. E2E: degraded mode → constitutional rules still fire without cognitive data.
- [ ] CLM-T27. E2E: evaluation provenance → case + eval record + basis reproducible.

---

## 14. What This Does NOT Cover (v1)

- Content moderation (hate speech, inappropriate content) — separate concern.
- Financial transaction enforcement — separate concern.
- AI-assisted normative evaluation — deferred to future hybrid mode.
- Cross-hive normative coordination — deferred.
- Full constitutional document management — deferred (see `identity-v3-direction.md`).
- Complex delegation chains (principal_ilk fully resolved) — v1 supports the field but does not trace delegation graphs.
- Compound cases (multi-message violations) — deferred.
- Cross-message correlation for patterns — deferred beyond reincidence tracking.

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
