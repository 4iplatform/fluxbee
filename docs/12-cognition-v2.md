# Fluxbee - 12 Cognition (SY.cognition) v2

**Status:** v2.0 (final)
**Date:** 2026-03-31
**Audience:** All developers (cognitive, AI, IO, router, identity)
**Replaces:** `12-cognition.md` (v1.16)
**Sources:** Cognitive Lab audit (`15-auditoria-tecnica-especifica-independiente.md`), architecture sessions, operational reason design

---

## 1. Purpose

SY.cognition is the cognitive engine of Fluxbee. It processes every message that flows through the system and builds structured understanding over time through two parallel corrients:

- **What is being discussed** (semantic context)
- **What the communication seeks to cause** (operational reason)

Both corrients are born from the same message, live in parallel with the same mechanics, and fuse when consolidated into memory.

Its responsibilities are:

1. **Tag messages** — extract semantic tags AND reason signals from each message.
2. **Maintain contexts** — track active topics with weight, decay, and ILK participation.
3. **Maintain reasons** — track active operational drivers with the same mechanics.
4. **Detect scope transitions** — identify when conversational cohesion shifts (using both context AND reason similarity).
5. **Generate memories** — produce narrative summaries that fuse what was discussed with why it was discussed.
6. **Detect episodes** — identify significant socio-emotional events with a conservative gate.
7. **Serve cognitive data** — make contexts, reasons, memories, and episodes available for message enrichment.

SY.cognition does NOT handle:

- Immediate conversation memory (AI SDK responsibility).
- Identity management (SY.identity).
- Message persistence to PostgreSQL (SY.storage).
- Routing decisions (router + OPA). Cognitive data does NOT feed OPA.
- Normative evaluation (SY.policy responsibility). Cognitive does NOT judge compliance.

---

## 2. The Double Helix: Context and Reason

### 2.1 Concept

Every message carries two dimensions of information:

**Context (semantic field):** What is this about? What topic persists? What subject gains or loses weight?

**Reason (operational direction):** What does this message seek to cause? What drive does it carry? What kind of movement does it attempt to produce in the system?

These two dimensions are:
- **Born together** — extracted from the same message in a single tagger call.
- **Tracked in parallel** — same mechanics (score, weight, decay, ILK profiles) but answering different questions.
- **Fused in memory** — when consolidated, memory captures both the topic and the recurring drive behind it.

### 2.2 Why Two Corrients

Neither alone is sufficient:

- Context without reason: "Juan discussed billing" — but was he complaining, seeking resolution, providing information, or threatening to leave?
- Reason without context: "Someone was seeking resolution" — but about what?

Together: "Juan brings billing recurrently as a complaint seeking urgent resolution." That is operational experience, not just topic archival.

### 2.3 Separation Discipline

While alive in caliente (per message, per thread), contexts and reasons are separate objects. They are NOT mixed prematurely. This prevents the common error of conflating topic, emotion, intent, and action into a single muddy tag.

They fuse only at memory consolidation time, when the summarizer has enough evidence to produce a meaningful narrative that includes both.

---

## 3. The Six Core Entities

### 3.1 Thread

The physical unit of conversational continuity given by the medium. Created by the IO node.

**Thread is purely physical/relational.** It does not depend on topic, context, or intention. It represents the communication structure of the medium: the channel, the relationship between interlocutors, and the sequencing that the medium imposes.

**Identity:** `thread:<hash>` — deterministic, computed by the IO node based on channel type:

| Channel type | Computation | Example |
|-------------|-------------|---------|
| DM / 1:1 | `hash(sorted(peer_ilks) + ich_id)` | WhatsApp 1:1 chat, email between A and B |
| Group / persistent channel | `hash(ich_id)` | WhatsApp group, Slack channel. Roster changes do not change the thread |
| Medium-native thread | `hash(native_thread_id + ich_id)` | Email In-Reply-To, Slack thread reply, helpdesk ticket ID |

The IO node chooses the rule because it knows the channel type. The SDK provides a canonical `compute_thread_id(channel_type, params)` function.
The current canonical format uses versioned input material plus stable `sha256`, producing identifiers like `thread:sha256:<hex>`.

**Sequencing:** every message that belongs to a thread also carries `meta.thread_seq`. Unlike `thread_id`, this value is **not** assigned by the IO node. It is assigned by the router when the message enters the local conversation flow for that thread, so the system has a deterministic per-thread order for persistence, replay, scope cuts, and evidence ranges.

**Rules:**

- Thread does not change when the topic changes. Semantic segmentation happens at scope/context level, not thread level.
- Thread can be long-lived (months, years). That is a feature, not a bug — the meaning is tracked by contexts, reasons, and scopes, not by the thread itself.
- For groups, participants entering or leaving do not create a new thread. The channel identity (ICH) is stable.
- All messages, contexts, reasons, memories, and episodes are associated with a thread.
- `thread_id` is computed by SDK/IO. `thread_seq` is assigned by the router.

### 3.2 Scope

The abstract unit of conversational continuity. Emerges from accumulated context AND reason.

**Identity:** `scope:<uuid-v4>`. Generated by SY.cognition when binding energy detects stable cohesion.

**Scope Instance:** Temporal window within a scope. Cuts when binding energy indicates sustained shift — considering both thematic and reason-driven divergence.

### 3.3 Context

A semantic topic tracked over time. Answers: "what is being discussed."

**Identity:** `context:<uuid-v4>`.

**Mode:** Additive only. No label refinement. Subtopics create new contexts.

```json
{
  "context_id": "context:uuid",
  "thread_id": "thread:hash",
  "scope_id": "scope:uuid",
  "label": "billing dispute",
  "status": "open",
  "score": 7.8,
  "weight": 4.2,
  "weight_avg_cumulative": 2.85,
  "weight_avg_ema": 2.85,
  "weight_samples": 23,
  "tags": ["billing", "refund", "invoice"],
  "ilk_weights": { "ilk:uuid-juan": 6.0 },
  "ilk_profile": { "ilk:uuid-juan": { "as_sender": 12, "as_receiver": 3 } },
  "opened_at": "...",
  "last_seen_at": "..."
}
```

### 3.4 Reason

An operational driver tracked over time. Answers: "what does the communication seek to cause."

**Identity:** `reason:<uuid-v4>`.

**Same structural form as Context, independent parameters.** Reasons have score, weight, decay, ILK weights, ILK profile — same shape. But they run with their own parameter set (thresholds, decay rates, EMA alpha) because reasons and contexts have different temporal dynamics. A context like "billing" can persist for weeks. A reason like "challenge" can appear and vanish in 3 messages.

Contexts and reasons are concurrent but not connected — they share only the originating message. They do not influence each other's weights or decay. They meet only at memory consolidation time.

```json
{
  "reason_id": "reason:uuid",
  "thread_id": "thread:hash",
  "scope_id": "scope:uuid",
  "label": "seeking urgent resolution",
  "status": "open",
  "score": 8.1,
  "weight": 3.8,
  "weight_avg_cumulative": 3.2,
  "weight_avg_ema": 3.2,
  "weight_samples": 18,
  "signals_canonical": ["resolve", "challenge"],
  "signals_extra": ["urgency", "frustration"],
  "ilk_weights": { "ilk:uuid-juan": 7.0 },
  "ilk_profile": { "ilk:uuid-juan": { "as_sender": 15, "as_receiver": 1 } },
  "opened_at": "...",
  "last_seen_at": "..."
}
```

### 3.5 Memory

A narrative summary that fuses context + reason. Answers: "what does the system know about this, and with what recurring drive."

**Identity:** `memory:<uuid-v4>`.

Memory is narrative, not exact. Like human memory, it captures the essence with accumulated perspective, not a precise replay.

```json
{
  "memory_id": "memory:uuid",
  "thread_id": "thread:hash",
  "scope_id": "scope:uuid",
  "summary": "Juan brings billing issues recurrently as complaints seeking urgent resolution. He has a pattern of escalating when response time exceeds his expectation.",
  "weight": 2.5,
  "occurrences": 3,
  "dominant_context_id": "context:uuid",
  "dominant_reason_id": "reason:uuid",
  "ilk_weights": { "ilk:uuid-juan": 5.0 },
  "created_at": "...",
  "last_seen_at": "..."
}
```

The `summary` is the narrative fusion. The `dominant_context_id` and `dominant_reason_id` are structured metadata available for AI node enrichment via memory_package. OPA does not read these fields. SY.policy may read them as supporting evidence in the future but does not depend on them in v1.

### 3.6 Episode

A significant socio-emotional event with intensity and evidence. Episodes are more precise than memories — they capture specific moments.

**Identity:** `episode:<uuid-v4>`.

Same structure as in the cognitive lab audit. Conservative gate: only persisted when evidence is strong and unambiguous.

```json
{
  "episode_id": "episode:uuid",
  "thread_id": "thread:hash",
  "scope_id": "scope:uuid",
  "affect_id": "anger",
  "title": "Friction over delayed refund",
  "summary": "Customer expressed strong frustration about refund processing time.",
  "base_intensity": 8,
  "evidence_strength": 9,
  "evidence_context_ids": ["context:uuid"],
  "evidence_reason_ids": ["reason:uuid"],
  "evidence_signals": ["anger", "complaint"],
  "intensity": 8,
  "reason": "Explicit friction in dominant context with urgent resolution drive"
}
```

---

## 4. Reason Signals: The Commandments

### 4.1 Philosophy

Reason signals are NOT detailed action categories. They are fundamental, general behavioral principles — like commandments. The system should recognize basic human drives in communication without micromanaging intent classification.

### 4.2 The Base Commandments (v1)

These are the minimum reason signals the tagger should recognize. They are intentionally general and few:

| Signal | Meaning |
|--------|---------|
| `resolve` | Seeking solution to a problem |
| `inform` | Giving or seeking information |
| `protect` | Defending from harm, guarding interests |
| `connect` | Establishing or maintaining relationship |
| `challenge` | Questioning, opposing, confronting |
| `confirm` | Validating, agreeing, acknowledging |
| `request` | Asking for something specific |
| `abandon` | Withdrawing, giving up, disengaging |

### 4.3 Rules

- The tagger extracts reason signals from the same message, in the same call as semantic tags.
- A message can carry multiple reason signals (a complaint that also seeks resolution).
- **The 8 commandments are a closed canonical set for v1.** The cognitive reason evaluator only operates on these 8 signals. This ensures deterministic processing and stable SHM indexing.
- If the tagger detects nuances beyond the 8, they go to a separate `reason_signals_extra` field as free-text evidence. This evidence is available for narrative memory generation but does NOT feed the reason evaluator.
- The reason evaluator (same mechanics as context evaluator) groups canonical signals into reasons with labels, weights, and ILK profiles.
- Reasons do not refine labels (same additive rule as contexts).
- **OPA does not read reason signals.** SY.policy may read them independently from the message stream for normative evaluation, but that is SY.policy's concern, not cognitive's.

**Canonical kernel vs periferia:**

```json
{
  "reason_signals_canonical": ["resolve", "challenge"],
  "reason_signals_extra": ["urgency", "frustration", "implicit_threat"],
  "reason_label": "seeking urgent resolution"
}
```

The canonical signals feed the cognitive pipeline (reason evaluator → reasons → binding energy → memory fusion). The extra signals enrich narrative memory only.

### 4.4 Why General, Not Specific

Specific categories (`request_resolution`, `complaint`, `escalation_attempt`) are tempting but create a catalog maintenance problem and force the system to classify intent with more precision than is useful in real-time.

General commandments let the AI interpret freely while giving the deterministic engine enough structure to weight, decay, and correlate. The specificity comes later, in the narrative memory, not in the signal classification.

---

## 5. Pipeline

### 5.1 Overview

```
Message arrives (via NATS from router)
  │
  ▼
1. TAGGER (single AI call)
   Extract semantic tags AND reason signals.
   Input: message text + word_count
   Output: tags[] + reason_signals[]
  │
  ├─── tags feed ──────────────► CONTEXT EVALUATOR (AI)
  │                                Assign/create/close contexts
  │
  └─── reason_signals feed ────► REASON EVALUATOR (AI or deterministic)
                                   Assign/create/close reasons
  │
  ▼
3. DETERMINISTIC UPDATE (parallel for both)
   Update context weights, ILK profiles, decay.
   Update reason weights, ILK profiles, decay.
   Apply MAX_OPEN guardrails.
   Emit events.
  │
  ▼
4. SCOPE BINDING EVALUATION
   Calculate binding energy using BOTH context AND reason similarity.
   Update EMA, evaluate threshold, detect scope change.
  │
  ▼
5. PERIOD DETECTION
   Build snapshots with main context + dominant reason.
   Cut periods by switch, decay, or time threshold.
  │
  ▼
6. SCOPE SUMMARIZER (single AI call per period)
   Receives contexts AND reasons for the period.
   Generates narrative memories that fuse both.
   Evaluates episode decisions.
  │
  ▼
7. APPLY RESULTS
   Create/update memories (narrative fusion of context+reason).
   Persist episodes that pass gate.
   Update LanceDB + jsr-memory SHM.
  │
  ▼
8. PUBLISH TO NATS (fire & forget)
```

### 5.2 Where Cognitive Gets Messages

SY.cognition consumes from NATS (`storage.turns` stream). Same stream as SY.storage. Fire & forget from router (~1ms).

The canonical turn carrier for cognition v2 is:
- `meta.thread_id`
- `meta.thread_seq`
- `meta.ich`
- `meta.src_ilk`
- `meta.dst_ilk` when known

`meta.ctx`, `meta.ctx_seq`, and `meta.ctx_window` belong to the previous model and should be treated as legacy compatibility fields during migration, not as the canonical cognition carrier.

### 5.2.1 Canonical `meta` fields for cognition v2

The message protocol may still contain legacy fields during migration, but the **canonical** cognition carrier is:

```json
{
  "meta": {
    "type": "user",
    "src_ilk": "ilk:...",
    "dst_ilk": "ilk:...",
    "ich": "ich:...",
    "thread_id": "thread:...",
    "thread_seq": 47,
    "memory_package": {
      "package_version": 2,
      "...": "..."
    }
  }
}
```

Rules:
- `thread_id` is computed by SDK/IO.
- `thread_seq` is assigned by the router.
- `thread_seq` is monotonic **within a thread**, not globally.
- `ctx`, `ctx_seq`, and `ctx_window` are not part of the v2 canonical model.
- During migration, old producers/consumers may still read `ctx*`, but new canonical paths should not depend on them.

Temporary migration note:
- the router may still fallback from missing `meta.thread_id` to legacy `meta.context.thread_id` for continuity and `thread_seq` assignment.
- this fallback is transitional only and now remains only in router/core; repo IO and AI nodes already use top-level canonical fields.

### 5.3 Message Enrichment

Router reads `jsr-memory-<hive>` SHM to attach `memory_package` to messages. The router enriches, not SY.cognition. SY.cognition writes the data; the router reads and attaches it.

Current implementation note:
- `jsr-memory-<hive>` is implemented as a seqlock-protected SHM region with a versioned JSON snapshot.
- the lookup key is `thread_id`
- each snapshot entry already contains a truncated `memory_package` v2 for that thread

**Important:** Enrichment happens AFTER the routing decision. OPA decides where the message goes based on identity data only. Then, before delivery to the destination node, the router attaches the memory_package so the AI node has cognitive context. The memory_package does not influence routing.

In v2, router enrichment should be resolved primarily by `thread_id` (and optionally narrowed by message identity metadata), not by semantic tags of the current message. This keeps the enrichment path viable even when cognitive tagging is asynchronous.

---

## 6. AI Contracts

### 6.1 Tagger (Extended)

Single AI call produces both semantic tags and reason signals.

**Input:**

```json
{
  "word_count": 18,
  "max_tags": 12,
  "max_reason_signals": 4,
  "canonical_signals": ["resolve", "inform", "protect", "connect", "challenge", "confirm", "request", "abandon"],
  "text": "Message text"
}
```

**Output:**

```json
{
  "tags": ["billing", "refund", "complaint"],
  "reason_signals_canonical": ["resolve", "challenge"],
  "reason_signals_extra": ["urgency", "frustration"]
}
```

**Tag post-processing:** Same as before (lowercase, dedup, limit).

**Reason signal post-processing:**
- `reason_signals_canonical`: filter to only values present in the canonical set. Lowercase, dedup.
- `reason_signals_extra`: any additional signals detected. Stored as evidence, does not feed reason evaluator.

**ILK metadata (added by backend, NOT by tagger):**

The tagger outputs plain strings only — it does not know who sent the message. The backend enriches each tag and signal with ILK metadata from the message before feeding them to the evaluators:

```json
{
  "value": "billing",
  "ilk_sender": "ilk:uuid",
  "ilk_receiver": "ilk:uuid"
}
```

This enriched form is what the Context Evaluator and Reason Evaluator receive. The tagger never sees ILK data.

### 6.2 Context Evaluator

Unchanged from cognitive lab audit. Tags-only input, additive mode.

### 6.3 Reason Evaluator

Same contract shape as Context Evaluator but for reason signals:

**Input:**

```json
{
  "REASON_SCORE_THRESHOLD": 4,
  "signals": [{"value": "resolve"}, {"value": "challenge"}],
  "open_reasons": [
    {
      "reason_id": "reason:uuid",
      "label": "seeking urgent resolution",
      "score": 7.2,
      "weight": 3.1,
      "status": "open"
    }
  ],
  "identity": {
    "sender_ilk": "ilk:uuid",
    "recipient_ilks": ["ilk:uuid"]
  }
}
```

**Output:**

```json
{
  "assignments": [
    {"reason_id": "reason:uuid", "label": "seeking urgent resolution", "score": 7.8}
  ],
  "new_reasons": [
    {"label": "confrontational pushback", "score": 6.5}
  ],
  "to_close": []
}
```

Same rules as Context Evaluator: score threshold, additive mode, no label refinement.

**Implementation note:** For v1, the Reason Evaluator can be a second AI call or deterministic mapping from signals to reasons. If signals are general enough (the commandments), deterministic mapping may suffice initially.

### 6.4 Scope Summarizer (Extended)

Single AI call per period. Now receives contexts AND reasons.

**Input:**

```json
{
  "period": {"start_ts": "...", "end_ts": "..."},
  "main_context": {"context_id": "context:uuid", "label": "billing dispute"},
  "dominant_reason": {"reason_id": "reason:uuid", "label": "seeking urgent resolution"},
  "memories": [...],
  "affects": [...],
  "prior_stats": [...],
  "contexts": [...],
  "reasons": [
    {
      "reason_id": "reason:uuid",
      "label": "seeking urgent resolution",
      "weight": 3.8,
      "weight_avg_ema": 3.2
    }
  ]
}
```

**Output:** Same structure as before, but the AI is instructed to produce narrative memories that fuse context and reason:

```json
{
  "period_memories": [
    {
      "extracto": "Juan brings billing issues recurrently as complaints seeking urgent resolution. He escalates when response time exceeds expectations.",
      "memory_match_id": "memory:uuid",
      "weight_delta": 1.2,
      "enrich": true
    }
  ],
  "global_adjustments": [...],
  "episode_decision": {
    "should_create_episode": true,
    "affect_id": "anger",
    "evidence_context_ids": ["context:uuid"],
    "evidence_reason_ids": ["reason:uuid"],
    "evidence_signals": ["anger", "resolve"],
    "evidence_strength": 9,
    "reason": "Explicit friction in billing context driven by urgent resolution demand"
  }
}
```

---

## 7. Scope Binding Energy (Extended)

### 7.1 Two-Variable Binding

Binding energy now considers BOTH context similarity AND reason similarity between active objects.

**Step 1 — Context similarity (Jaccard of tags, as before):**

```
S_ctx(i,j) = |tags_i ∩ tags_j| / |tags_i ∪ tags_j|
```

**Step 2 — Reason similarity (Jaccard of canonical signals):**

```
S_rsn(i,j) = |signals_canonical_i ∩ signals_canonical_j| / |signals_canonical_i ∪ signals_canonical_j|
```

Only canonical signals feed the binding energy calculation. Extra signals are evidence for memory, not for binding.

**Step 3 — ILK similarity (Cosine, unchanged):**

```
S_ilk(i,j) = cosine(vec_i, vec_j)
```

**Step 4 — Coupling (now three factors):**

```
J(i,j) = S_ctx(i,j) × S_rsn(i,j) × S_ilk(i,j)
```

When contexts and reasons are aligned (same topic AND same drive AND same participants), coupling is strong. When any dimension diverges, coupling weakens.

**Steps 5-7** (energy, normalization, EMA, threshold): unchanged.

### 7.2 What This Means for Scope

A scope stays stable when people discuss similar topics with similar drives. A scope changes when:

- Topics shift (different contexts) — already detected in v1.
- Drives shift (same topic but different operational purpose) — NEW in v2.
- Both shift — strongest signal.

Example: "billing" discussed as complaint (challenge + resolve) vs "billing" discussed as routine inquiry (inform + confirm) — same context, different reasons. The binding energy detects this as a potential scope shift even though the topic hasn't changed.

---

## 8. Deterministic Algorithms

### 8.1 Context Update Per Message

Unchanged from cognitive lab audit. Score threshold, weight update, ILK profiles, decay, MAX_OPEN_CONTEXTS guardrail.

### 8.2 Reason Update Per Message

Same structural algorithm as Context Update, but using **reason-specific parameters** (`RSN_SCORE_THRESHOLD`, `RSN_DECAY_FACTOR`, `RSN_EMA_ALPHA`, `RSN_MAX_OPEN`, `RSN_ILK_WEIGHT_*`):

1. Resolve assignments and new reasons from evaluator.
2. Update weight and averages for each open reason (using `RSN_EMA_ALPHA`, `RSN_DECAY_FACTOR`).
3. Close if weight <= 0.
4. Update ILK weights and profiles if identity present (using `RSN_ILK_WEIGHT_*`).
5. Apply `RSN_MAX_OPEN` guardrail.

Context update and reason update run concurrently on the same message but do not influence each other. They share no state — only the originating message.

### 8.3 Scope Binding Energy

Extended with reason similarity as described in section 7.

### 8.4 Period Detection

Now builds snapshots with main context AND dominant reason. Periods cut when either context or reason switches significantly.

### 8.5 Memory Application

Per period. AI returns narrative memories that fuse context+reason. Application mechanics unchanged (match → update, no match → create, decay unreinforced).

### 8.6 Episode Gate

Unchanged from audit. Gate conditions remain the same. Evidence now includes `evidence_reason_ids` in addition to `evidence_context_ids`.

---

## 9. Parameters and Thresholds

### 9.1 Contexts

| Parameter | Value |
|-----------|-------|
| `CONTEXT_SCORE_THRESHOLD` | 4 |
| `MAX_TAGS` | 12 |
| `MAX_OPEN_CONTEXTS` | 20 |

### 9.2 Reasons

Reasons use the same structural form as contexts (score, weight, decay, EMA, ILK profiles) but with **independent parameters**. This is intentional: a topic can persist for weeks while an operational drive can shift in 3 messages. The tempo of context and the tempo of reason are different.

| Parameter | Value | Note |
|-----------|-------|------|
| `REASON_SCORE_THRESHOLD` | 4 | May diverge from context threshold as tuning reveals different sensitivities |
| `REASON_DECAY_FACTOR` | 1.5 | Faster decay than context — reasons are more volatile |
| `MAX_REASON_SIGNALS` | 4 | Per message |
| `MAX_OPEN_REASONS` | 10 | Lower than MAX_OPEN_CONTEXTS (20) because reasons are fewer and more general |
| `REASON_ILK_WEIGHT_SENDER` | 1.0 | May diverge from context ILK weights |
| `REASON_ILK_WEIGHT_RECIPIENT` | 0.5 | May diverge from context ILK weights |
| `REASON_EMA_ALPHA` | 0.3 | Faster EMA than context (0.25) — reasons respond to recent signal more quickly |

**Design rule:** "Same mechanics" means same structural form (score, weight, decay, ILK profiles, EMA). It does NOT mean same parameter values. Context and reason parameters MUST be independently tunable because they track phenomena with different temporal dynamics.

### 9.3 ILK Weights

| Parameter | Value |
|-----------|-------|
| `CTX_ILK_WEIGHT_SENDER` | 1.0 |
| `CTX_ILK_WEIGHT_RECIPIENT` | 0.5 |
| `RSN_ILK_WEIGHT_SENDER` | 1.0 |
| `RSN_ILK_WEIGHT_RECIPIENT` | 0.5 |

Context and reason ILK weights start identical but are independently configurable.

### 9.4 Scope Binding

| Parameter | Value |
|-----------|-------|
| `SCOPE_ENERGY_ALPHA` | 0.25 |
| `SCOPE_ENERGY_UNBIND_THRESHOLD` | -0.05 |
| `SCOPE_ENERGY_MIN_CTX_FOR_EVAL` | 2 |
| `SCOPE_ENERGY_SUSTAIN_COUNT` | 2 |

### 9.5 Memories

| Parameter | Value |
|-----------|-------|
| `MEMORY_DECAY_STEP` | 0.25 |

### 9.6 Episodes

| Parameter | Value |
|-----------|-------|
| `EPISODE_MIN_FINAL_INTENSITY` | 7 |
| `EPISODE_MIN_EVIDENCE_STRENGTH` | 8 |

---

## 10. Architecture

### 10.1 One Per Hive

SY.cognition runs on every hive. Each instance processes messages from its local NATS stream.

### 10.2 Storage Layers

| Layer | Purpose | Location | Latency |
|-------|---------|----------|---------|
| jsr-memory SHM | Index for router enrichment | `/dev/shm/jsr-memory-<hive>` | ~μs |
| LanceDB | Local cognitive database | `/var/lib/fluxbee/nodes/SY/SY.cognition@<hive>/memory.lance` | ~ms |
| PostgreSQL | Durable source of truth (via SY.storage) | Motherbee only | ~10ms |

### 10.2.1 Durable Model (v2)

The durable model for cognition v2 should be explicit and should not reuse the old `events/items/reactivation` semantics as its conceptual base.

`storage.turns` remains the immutable input stream. Everything else belongs to the cognition v2 domain.

Recommended durable entities:

- `cognition_threads`
- `cognition_contexts`
- `cognition_reasons`
- `cognition_scopes`
- `cognition_scope_instances`
- `cognition_memories`
- `cognition_episodes`

Optional auxiliary durable entities:

- `cognition_context_signals`
- `cognition_reason_signals`
- `cognition_links`

Durable source-of-truth policy:

- **Source of truth**
  - turns
  - contexts
  - reasons
  - scopes
  - scope_instances
  - memories
  - episodes
- **Derived / rebuildable**
  - `jsr-memory`
  - local enrichment indexes
  - local hot caches

Recommended transport to `SY.storage`:

- explicit cognition subjects, not the old v1 semantic subjects
- for example:
  - `storage.cognition.threads`
  - `storage.cognition.contexts`
  - `storage.cognition.reasons`
  - `storage.cognition.cooccurrences`
  - `storage.cognition.scopes`
  - `storage.cognition.scope_instances`
  - `storage.cognition.memories`
  - `storage.cognition.episodes`

This keeps the domain typed, inspectable, and migration-safe.

### 10.2.2 NATS Transport Envelope for Cognition v2

The transport contract to `SY.storage` should be explicit and uniform across cognition entities.

Recommended envelope:

```json
{
  "schema_version": 1,
  "entity_version": 1,
  "entity": "context",
  "op": "upsert",
  "entity_id": "context:uuid",
  "thread_id": "thread:sha256:...",
  "hive": "motherbee",
  "writer": "SY.cognition@motherbee",
  "ts": "2026-03-31T12:34:56Z",
  "data": {
    "...": "entity payload"
  }
}
```

Rules:
- subject defines the domain stream; `entity` and `entity_id` make the payload self-describing.
- `thread_id` is required for thread-scoped entities (`threads`, `contexts`, `reasons`, `cooccurrences`, `scope_instances`, `memories`, `episodes`).
- `op` should start as a closed deterministic set for v1:
  - `upsert`
  - `close`
  - `delete` only if a real deletion contract is later approved
- `schema_version` is the transport version.
- `entity_version` is the durable shape version for that entity.
- `writer` identifies the emitting node instance and helps trace rebuild/drift issues.

Router / JetStream boundary:
- the router publishes only the immutable input stream (`storage.turns`).
- the router is not the writer of derived cognition entities.
- `storage.cognition.*` subjects belong to the `SY.cognition -> SY.storage` durable path.
- the embedded broker/JetStream layer must therefore support both:
  - durable replay of `storage.turns` for `SY.storage` and `SY.cognition`
  - durable replay of `storage.cognition.*` for storage-side ingestion/rebuild flows

Open transport items still to freeze in implementation:
- canonical durable consumer names / queue names
- replay/start semantics after restart
- ack/redelivery/poison-message policy

Recommended subject-to-entity mapping:
- `storage.cognition.threads` → thread snapshots / thread-level state
- `storage.cognition.contexts` → context entities
- `storage.cognition.reasons` → reason entities
- `storage.cognition.cooccurrences` → thread-scoped context/reason coupling records
- `storage.cognition.scopes` → scope definitions
- `storage.cognition.scope_instances` → realized thread-local scope instances
- `storage.cognition.memories` → narrative memories
- `storage.cognition.episodes` → affective/episode records

Turns remain special:
- `storage.turns` is append-only immutable input
- cognition entities are durable derived state
- rebuilding cognition means replaying `storage.turns` into these typed cognition subjects / tables

### 10.3 Cold Start

Rebuild from PostgreSQL via SY.storage socket. Replay turns through pipeline to reconstruct contexts, reasons, memories, episodes.

### 10.4 SHM Region: jsr-memory-\<hive\>

Single writer: SY.cognition. Uses seqlock.

Contains hot cognitive summaries keyed for router enrichment. In v2 the primary lookup axis is `thread_id`, with compact references to:
- dominant context
- dominant reason
- top contexts
- top reasons
- top memories
- top episodes

The SHM is for fast enrichment lookups, not for full evidence replay.

### 10.4.1 Logical Layout of `jsr-memory`

The logical layout should prioritize deterministic, bounded router reads.

Recommended contents:

1. **Header**
   - magic
   - version
   - owner UUID
   - seqlock sequence
   - counters
   - timestamps

2. **Thread Index**
   - `thread_id -> thread_slot`
   - fixed-size or bounded hash table

3. **Thread Summary Entries**
   - `thread_id`
   - latest `thread_seq`
   - dominant context ref
   - dominant reason ref
   - top-N context refs
   - top-N reason refs
   - top-N memory refs
   - top-N episode refs
   - updated_at

4. **Compact Entity Pools**
   - context summary pool
   - reason summary pool
   - memory summary pool
   - episode summary pool

5. **Optional reverse index**
   - for future use only
   - no semantic nearest-neighbor logic required in v1

Design constraints:

- single writer: `SY.cognition`
- lock-free readers: router and local diagnostics
- no large text bodies in SHM
- summaries only; full evidence lives in durable storage / local DB
- all arrays bounded with explicit truncation rules

Recommended first read path:

- router receives message with `thread_id`
- router probes `jsr-memory` by `thread_id`
- router builds `memory_package`
- router never performs heavy semantic search in hot path

---

## 11. Memory Package (Router Enrichment for AI Nodes)

The memory_package is attached by the router to messages before delivery to destination AI nodes. It provides the AI with conversational understanding accumulated over time. **The memory_package is NOT used for routing decisions.** OPA does not see it. Routing happens before enrichment.

```json
{
  "meta": {
    "memory_package": {
      "package_version": 2,
      "thread_id": "thread:...",
      "dominant_context": {
        "context_id": "...",
        "label": "billing dispute",
        "weight": 4.2
      },
      "dominant_reason": {
        "reason_id": "...",
        "label": "seeking urgent resolution",
        "weight": 3.8
      },
      "contexts": [
        {"context_id": "...", "label": "billing dispute", "weight": 4.2}
      ],
      "reasons": [
        {"reason_id": "...", "label": "seeking urgent resolution", "weight": 3.8}
      ],
      "memories": [
        {"memory_id": "...", "summary": "Juan brings billing issues as complaints...", "weight": 2.5,
         "dominant_context_id": "...", "dominant_reason_id": "..."}
      ],
      "episodes": [
        {"episode_id": "...", "title": "Friction over delayed refund", "intensity": 8}
      ],
      "truncated": {
        "applied": false,
        "dropped_contexts": 0,
        "dropped_reasons": 0,
        "dropped_memories": 0,
        "dropped_episodes": 0
      ]
    }
  }
}
```

Contract notes:
- `dominant_context` and `dominant_reason` are mandatory whenever the package is non-empty.
- `thread_id` inside the package must match `meta.thread_id`.
- `package_version=2` marks the new cognition model and separates it from the old event/highlight package shape.
- The package is resolved from `jsr-memory` by `thread_id`, not by an inline semantic query over the current message text.

### Limits

| Limit | Value |
|-------|-------|
| `MEMORY_PACKAGE_MAX_BYTES` | 32,768 |
| `MEMORY_PACKAGE_MAX_CONTEXTS` | 5 |
| `MEMORY_PACKAGE_MAX_REASONS` | 3 |
| `MEMORY_PACKAGE_MAX_MEMORIES` | 5 |
| `MEMORY_PACKAGE_MAX_EPISODES` | 3 |
| `MEMORY_FETCH_TIMEOUT_MS` | 50 |

### Truncation Priority

When the 32KB budget is exceeded, truncate in this order (lowest priority first):

1. **Drop episodes** beyond limit (keep highest intensity first).
2. **Drop reasons** beyond limit (keep highest weight first).
3. **Truncate memory summaries** to 200 chars each (keep full dominant_context_id / dominant_reason_id).
4. **Drop contexts** beyond limit (keep highest weight first).
5. **Drop memories** beyond limit (keep highest weight first).
6. **Never drop:** dominant context (highest weight context) and dominant reason (highest weight reason). These two are always present even if everything else is truncated.

The rationale: the destination AI node must always know at minimum what the conversation is about (dominant context) and what it seeks (dominant reason). Everything else is enrichment.

---

## 12. Naming: v1.16 to v2 Migration

| v1.16 Term | v2 Term | What Changed |
|------------|---------|--------------|
| `ctx:<hash>` | `thread:<hash>` | Thread is the physical conversation unit |
| Episode (block of turns) | Scope Instance | Temporal window cut by binding energy |
| Episode (event) | Episode | Socio-emotional event with gate |
| Event (episodic unit) | Context + Reason | Two parallel tracking entities |
| Memory Item | Memory | Narrative summary fusing context + reason |
| Cue/Tag | Tag + Signal | Tags for context, signals for reason |
| AI.tagger | Tagger | Internal to SY.cognition, produces both tags + signals |
| AI.consolidator | Scope Summarizer | Single AI call, receives contexts + reasons |
| activation_strength | weight | Same concept, simpler name |
| (not in v1.16) | Reason | NEW: operational driver entity |
| `fear_id` | `affect_id` | Renamed: not all episodic events are fear-based |

---

## 13. Open Design Questions

### 13.1 Scope Association Algorithm

How does SY.cognition match a new thread to an existing scope? By ILK overlap? ICH overlap? Minimum threshold?

### 13.2 Affects Catalog

Where does the catalog of affects (anger, frustration, satisfaction, conflict, etc.) live? For v1: hardcoded in SY.cognition. The field was previously named `fear_id` — renamed to `affect_id` because not all episodic events are fear-based.

### 13.3 AI Call Strategy

SY.cognition makes direct AI calls via AI SDK (same as SY.architect). Carries its own AI provider key in config.

### 13.4 Reason Evaluator: AI or Deterministic?

For v1, the reason evaluator is deterministic, operating only on the 8 canonical signals. It maps signals to reasons by keyword matching and grouping. Since the commandments are general and few, deterministic mapping is sufficient. Upgrade to AI call later if needed.

### 13.5 Thread ID Hash Function

**Resolved.** Thread computation uses three rules by channel type (see section 3.1). The SDK provides `compute_thread_id(channel_type, params)`. Current hash function/format: versioned canonical material + `sha256`, output as `thread:sha256:<hex>`.

**Additional resolved rule:** `thread_seq` is assigned by the router, not by IO. The router is the only component that should serialize message order within a thread for cognition and durable replay.

### 13.6 Feed/Stream Processing

Continuous feeds deferred. System assumes discrete messages. IO handles segmentation.

### 13.7 Relationship to SY.policy and OPA

**SY.cognition has no relationship to OPA.** OPA does not read any cognitive data — not contexts, not reasons, not memories, not episodes, not reason signals. OPA reads identity data (from jsr-identity SHM) and active restrictions (from SY.policy). That's it.

**SY.cognition has no direct relationship to SY.policy.** SY.policy is a separate NATS consumer that reads the same message stream as cognitive. SY.policy may independently read reason signals from the original message (the tagger output that travels in the message metadata) to inform its normative evaluation. But SY.policy does not depend on cognitive having processed the message first. They run in parallel, not in series.

**The architecture is:**

```
Message → NATS
  ├──► SY.cognition (describes: contexts, reasons, memories, episodes)
  │         └──► jsr-memory SHM (for router enrichment of AI nodes)
  │
  └──► SY.policy (evaluates: normative compliance, post-facto)
           └──► restrictions → jsr-identity SHM (for OPA to read)
```

Cognitive and policy are parallel consumers. Cognitive enriches AI nodes. Policy feeds OPA. They don't feed each other in v1.

**Future possibility:** SY.policy may eventually read cognitive output (memories, episodes) as supporting evidence for normative evaluation. But that is additive — cognitive does not change to accommodate policy, and policy does not depend on cognitive for its core function.

---

## 14. Relationship to Immediate Memory (AI SDK)

| Aspect | Immediate Memory (SDK) | Cognitive Memory (SY.cognition) |
|--------|------------------------|--------------------------------|
| Scope | Single session | Cross-session, cross-thread |
| Horizon | Last 10-12 interactions | Days, weeks, months |
| Owner | Each AI node | SY.cognition (system-wide) |
| Content | What am I doing right now | What do I know + with what drive |
| Consumed by | The AI node itself | Router (enrichment → AI nodes via memory_package). NOT consumed by OPA or SY.policy in v1 |

Both coexist. Neither replaces the other.

Operational alignment note:

- `thread_id` is conversational metadata for cognition and router enrichment
- AI runtime immediate memory and node-local hard state remain keyed by `src_ilk`
- repo AI runtimes now read only top-level `meta.thread_id` / `meta.src_ilk`
- any remaining legacy fallback is limited to router/core migration paths, not AI SDK thread-state tooling

---

## 15. Implementation Tasks

### Phase 1 — Contexts (Core Pipeline)

- [x] COG-T1. Implement `compute_thread_id(channel_type, params)` in SDK with three rules (DM, group, native).
- [ ] COG-T2. IO nodes include `thread_id` in message metadata using SDK function.
- [ ] COG-T2b. Router assigns `thread_seq` per `thread_id` and includes it in message metadata.
- [ ] COG-T3. SY.cognition consumes from NATS `storage.turns`.
- [ ] COG-T4. Implement tagger (extended: tags + reason_signals_canonical + reason_signals_extra in single call).
- [ ] COG-T5. Implement context evaluator.
- [ ] COG-T6. Implement deterministic context update.

### Phase 2 — Reasons (Parallel Corrient)

- [ ] COG-T7. Implement reason evaluator (deterministic v1: canonical signals only, map to reasons).
- [ ] COG-T8. Implement deterministic reason update (same mechanics as context).
- [ ] COG-T9. Track co-occurrences between contexts and reasons.

### Phase 3 — Scope and Binding

- [ ] COG-T10. Implement scope creation and thread-scope association.
- [ ] COG-T11. Implement binding energy with two-variable coupling (context + reason similarity).
- [ ] COG-T12. Implement energy EMA and scope instance transitions.
- [ ] COG-T13. Emit scope events to NATS.

### Phase 4 — Memories (Narrative Fusion)

- [ ] COG-T14. Implement period detection (with main context + dominant reason).
- [ ] COG-T15. Implement scope summarizer (contexts + reasons → narrative memory).
- [ ] COG-T16. Implement memory creation, reinforcement, and decay.
- [ ] COG-T17. Store `dominant_context_id` and `dominant_reason_id` as structured metadata on memories.

### Phase 5 — Episodes

- [ ] COG-T18. Implement episode gate (conservative, multi-condition).
- [ ] COG-T19. Include `evidence_reason_ids` in episode evidence.
- [ ] COG-T20. Publish memory and episode events to NATS.

### Phase 6 — Storage and SHM

- [ ] COG-T21. Implement LanceDB schema for threads, contexts, reasons, memories, episodes.
- [x] COG-T22. Implement jsr-memory SHM writer (seqlock, versioned snapshot by `thread_id`).
- [x] COG-T23. Implement cold start (rebuild from PostgreSQL via SY.storage).

### Phase 7 — Router Enrichment

- [x] COG-T24. Router reads jsr-memory and builds memory_package (includes reasons).
- [x] COG-T25. Router attaches memory_package to messages before delivery.
- [x] COG-T26. Respect memory_package limits.

### Phase 8 — E2E Validation

Validation note:
- The canonical E2E for cognition v2 must enter through the router path with real/disposable nodes.
- The old direct publish-to-`storage.turns` smoke was removed from the repo to avoid confusing it with the normative path.
- PostgreSQL is not the primary oracle for the cognition E2E. The primary oracle is delivery to the destination node plus `jsr-memory` and `SY.cognition` runtime counters.
- Cold start rebuild is startup-only and fail-open: rebuild is attempted only when local cognition state is empty, and missing/unreachable durable storage does not block live processing.
- Deferred design note:
  - the current rebuild path is hive-scoped, not volume-bounded
  - the next design pass should define a bounded rebuild / SHM hot set policy
  - first filter: live threads (`ILK` / `ICH` active) and/or open `scope_instances`
  - second filter: time window by recency (`last_seen_at` / `updated_at`)
  - `jsr-memory` should eventually carry only a bounded hot set sized to SHM capacity, not the full durable universe

- [ ] COG-T27. E2E: message → tagger → context + reason created.
- [ ] COG-T28. E2E: binding energy with context + reason → scope transition.
- [ ] COG-T29. E2E: period → summarizer → narrative memory fusing context + reason.
- [ ] COG-T30. E2E: episode with evidence from both context and reason.
- [ ] COG-T31. E2E: router enrichment → memory_package with reasons.
- [ ] COG-T32. E2E: cold start → rebuild from PostgreSQL.

### Phase 9 — Semantic Upgrade (Second Stage)

- [ ] COG-T33. Introduce `AI.tagger` as an operational upgrade under the same `tags + reason_signals_*` contract.
- [ ] COG-T34. Keep deterministic v1 tagger as fallback/runtime rollback path.
- [ ] COG-T35. Improve semantic extraction for tags:
  - paraphrases
  - implicit intent
  - soft entities from the message narrative
- [ ] COG-T36. Improve semantic extraction for canonical reason signals while preserving the closed 8-signal evaluator contract.
- [ ] COG-T37. Use `reason_signals_extra` as narrative evidence input for memory generation, not only as stored text.
- [ ] COG-T38. Upgrade the summarizer/memory generator from deterministic lexical fusion to semantic narrative synthesis.
- [ ] COG-T39. Build a golden corpus to compare deterministic v1 vs semantic v2 outputs.
- [ ] COG-T40. Define rollback semantics: if the AI provider is unavailable, cognition falls back to deterministic v1 without changing carrier or durable entities.

---

## 16. Constants

```rust
// Contexts
pub const CTX_SCORE_THRESHOLD: f64 = 4.0;
pub const CTX_MAX_TAGS: usize = 12;
pub const CTX_MAX_OPEN: usize = 20;
pub const CTX_ILK_WEIGHT_SENDER: f64 = 1.0;
pub const CTX_ILK_WEIGHT_RECIPIENT: f64 = 0.5;
// Context EMA uses SCOPE_ENERGY_ALPHA (0.25)

// Reasons (same structural form, independent parameter values)
pub const RSN_SCORE_THRESHOLD: f64 = 4.0;
pub const RSN_DECAY_FACTOR: f64 = 1.5;         // Faster decay than context
pub const RSN_MAX_SIGNALS: usize = 4;
pub const RSN_MAX_OPEN: usize = 10;
pub const RSN_ILK_WEIGHT_SENDER: f64 = 1.0;
pub const RSN_ILK_WEIGHT_RECIPIENT: f64 = 0.5;
pub const RSN_EMA_ALPHA: f64 = 0.3;            // Faster than context (0.25)

// Scope binding
pub const SCOPE_ENERGY_ALPHA: f64 = 0.25;
pub const SCOPE_ENERGY_UNBIND_THRESHOLD: f64 = -0.05;
pub const SCOPE_ENERGY_MIN_CTX_FOR_EVAL: usize = 2;
pub const SCOPE_ENERGY_SUSTAIN_COUNT: usize = 2;

// Memory
pub const MEMORY_DECAY_STEP: f64 = 0.25;
pub const MEMORY_PACKAGE_MAX_BYTES: usize = 32_768;
pub const MEMORY_PACKAGE_MAX_CONTEXTS: usize = 5;
pub const MEMORY_PACKAGE_MAX_REASONS: usize = 3;
pub const MEMORY_PACKAGE_MAX_MEMORIES: usize = 5;
pub const MEMORY_PACKAGE_MAX_EPISODES: usize = 3;
pub const MEMORY_FETCH_TIMEOUT_MS: u64 = 50;

// Episodes
pub const EPISODE_MIN_FINAL_INTENSITY: u8 = 7;
pub const EPISODE_MIN_EVIDENCE_STRENGTH: u8 = 8;

// SHM
pub const MEMORY_MAGIC: u32 = 0x4A534D45;  // "JSME"
pub const MEMORY_VERSION: u32 = 2;
```

---

## 17. References

| Topic | Document |
|-------|----------|
| Identity (ILK, ICH, TNT) | `10-identity-v2.md` |
| Storage (NATS, PostgreSQL) | `13-storage.md` |
| Protocol (message format) | `02-protocolo.md` |
| SHM regions | `03-shm.md` |
| Routing | `04-routing.md` |
| Scope binding energy (original) | `scope-binding-energy-spec.md` |
| Cognitive Lab audit | `15-auditoria-tecnica-especifica-independiente.md` |
| Immediate memory (AI SDK) | `immediate-conversation-memory-spec.md` |
| SY nodes catalog | `SY_nodes_spec.md` |
| Identity v3 direction (claims) | `identity-v3-direction.md` |
| Normative evaluation (SY.policy) | `sy-policy-beta.md` |
