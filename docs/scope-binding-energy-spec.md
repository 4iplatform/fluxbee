# Scope Detection by Binding Energy

**For:** cognitive-lab programmer  
**Goal:** Implement automatic detection of scope change based on the cohesion between active contexts  
**Dependency:** Requires CTX with tags weighted by ILK (sender/receiver)

---

## 1. The concept in one sentence

The scope is stable while the active contexts are "bound" to each other (they share topics and participants). When the contexts diverge enough and it holds, the scope changed.

---

## 2. Data structures

### 2.1 Tag (tagger output, modified)

Each tag now carries metadata of the message that generated it:

```json
{
  "tag": "facturación",
  "ilk_sender": "ilk:tenant:juan",
  "ilk_receiver": "ilk:agent:ai.soporte"
}
```

### 2.2 CTX (semantic context)

Each context accumulates tags and participation profiles:

```json
{
  "context_id": "ctx_001",
  "label": "problemas de facturación",
  "status": "open",
  "ema": 5.2,
  "weight": 7,
  "tags": ["facturación", "pago", "deuda", "mora"],
  "ilk_profile": {
    "ilk:tenant:juan":        { "as_sender": 12, "as_receiver": 3 },
    "ilk:agent:ai.soporte":   { "as_sender": 3,  "as_receiver": 12 }
  }
}
```

`ilk_profile` is updated with each message that touches this CTX:
- If the message comes from `ilk_sender=X` → `ilk_profile[X].as_sender += 1`
- If the message goes to `ilk_receiver=Y` → `ilk_profile[Y].as_receiver += 1`

### 2.3 Scope

```json
{
  "scope_id": "scope_001",
  "created_at": "2026-02-25T10:00:00Z",
  "last_activity_at": "2026-02-25T10:45:00Z",
  "energy_ema": -0.35,
  "contexts": ["ctx_001", "ctx_002", "ctx_003"],
  "participants": ["ilk:tenant:juan", "ilk:agent:ai.soporte"]
}
```

---

## 3. Step-by-step algorithm

It runs after the CTX are updated on each message.

### Step 1: Filter the active CTX

Only those with `ema > 0` (alive):

```typescript
const active = contexts.filter(c => c.ema > 0);
```

### Step 2: Compute the topic similarity between each pair

For each pair `(i, j)`:

```typescript
function tagSimilarity(ctx_i, ctx_j): number {
  const setI = new Set(ctx_i.tags);
  const setJ = new Set(ctx_j.tags);
  const intersection = [...setI].filter(t => setJ.has(t)).length;
  const union = new Set([...setI, ...setJ]).size;
  if (union === 0) return 0;
  return intersection / union;  // Jaccard: 0..1
}
```

### Step 3: Compute the participant similarity between each pair

Convert `ilk_profile` into a numeric vector and compute the cosine:

```typescript
function ilkSimilarity(ctx_i, ctx_j): number {
  // Union of all the ILKs present in both CTX
  const allIlks = new Set([
    ...Object.keys(ctx_i.ilk_profile),
    ...Object.keys(ctx_j.ilk_profile)
  ]);

  // Build vectors: for each ILK, add as_sender + as_receiver
  let dotProduct = 0;
  let normI = 0;
  let normJ = 0;

  for (const ilk of allIlks) {
    const pi = ctx_i.ilk_profile[ilk];
    const pj = ctx_j.ilk_profile[ilk];
    const vi = pi ? (pi.as_sender + pi.as_receiver) : 0;
    const vj = pj ? (pj.as_sender + pj.as_receiver) : 0;
    dotProduct += vi * vj;
    normI += vi * vi;
    normJ += vj * vj;
  }

  const denom = Math.sqrt(normI) * Math.sqrt(normJ);
  if (denom === 0) return 0;
  return dotProduct / denom;  // Cosine: 0..1
}
```

### Step 4: Compute the coupling J between each pair

```typescript
function coupling(ctx_i, ctx_j): number {
  return tagSimilarity(ctx_i, ctx_j) * ilkSimilarity(ctx_i, ctx_j);
}
```

Range: `0..1`. Zero = nothing in common. One = same tags and same participants.

### Step 5: Compute the total energy of the scope

```typescript
function scopeEnergy(active: CTX[]): number {
  let energy = 0;
  for (let i = 0; i < active.length; i++) {
    for (let j = i + 1; j < active.length; j++) {
      const J_ij = coupling(active[i], active[j]);
      const m_i = active[i].ema;
      const m_j = active[j].ema;
      energy += -J_ij * m_i * m_j;  // negative = bound
    }
  }
  return energy;
}
```

### Step 6: Normalize

```typescript
function normalizedEnergy(active: CTX[]): number {
  const E = scopeEnergy(active);
  const M = active.reduce((sum, c) => sum + c.ema, 0);
  if (M === 0) return 0;
  return E / (M * M);
}
```

Approximate range: `-1..0`
- `-1` = all the CTX strongly coupled (very stable scope)
- `0` = decoupled CTX (scope without cohesion)

### Step 7: Smooth with EMA

```typescript
const ALPHA = 0.25;

function updateEnergyEma(scope: Scope, currentEnergy: number): number {
  if (scope.energy_ema === undefined || scope.energy_ema === null) {
    return currentEnergy;  // first calculation
  }
  return scope.energy_ema + ALPHA * (currentEnergy - scope.energy_ema);
}
```

### Step 8: Evaluate the scope change

```typescript
const UNBIND_THRESHOLD = -0.10;  // to be calibrated

function shouldChangeScope(scope: Scope): boolean {
  return scope.energy_ema > UNBIND_THRESHOLD;
}
```

If `energy_ema > -0.10` → the scope lost cohesion → create a new scope.

---

## 4. Special case: a single active CTX

With a single CTX there are no pairs, the energy is 0 and the normalized energy is 0. This does not mean "scope change" — it means that the scope is just starting or that only one topic is left.

Rule: do not evaluate a scope change if `active.length < 2`. With a single CTX, the scope is kept.

---

## 5. What happens when the scope changes

```typescript
if (active.length >= 2 && shouldChangeScope(scope)) {
  // 1. Close the current scope
  scope.closed_at = now;
  scope.close_reason = "energy_unbind";
  scope.final_energy = scope.energy_ema;

  // 2. Create a new scope with the current active CTX
  const newScope = {
    scope_id: generateId(),
    created_at: now,
    energy_ema: null,  // will be computed on the next message
    contexts: active.map(c => c.context_id),
    participants: extractParticipants(active)
  };
}
```

---

## 6. Full step-by-step example

### Initial state: two bound CTX

```json
{
  "ctx_001": {
    "label": "facturación",
    "ema": 5.0,
    "tags": ["factura", "pago", "deuda"],
    "ilk_profile": {
      "ilk:tenant:juan":      { "as_sender": 8, "as_receiver": 2 },
      "ilk:agent:ai.soporte": { "as_sender": 2, "as_receiver": 8 }
    }
  },
  "ctx_002": {
    "label": "pagos pendientes",
    "ema": 3.0,
    "tags": ["pago", "mora", "cuenta"],
    "ilk_profile": {
      "ilk:tenant:juan":      { "as_sender": 5, "as_receiver": 1 },
      "ilk:agent:ai.soporte": { "as_sender": 1, "as_receiver": 5 }
    }
  }
}
```

**Tag similarity:**
```
tags_001 = {factura, pago, deuda}
tags_002 = {pago, mora, cuenta}
intersection = {pago} → 1
union = {factura, pago, deuda, mora, cuenta} → 5
S_tema = 1/5 = 0.20
```

**ILK similarity:**
```
vec_001 = [juan: 8+2=10, agente: 2+8=10]
vec_002 = [juan: 5+1=6,  agente: 1+5=6]
dot = (10*6) + (10*6) = 120
norm_001 = sqrt(100+100) = 14.14
norm_002 = sqrt(36+36) = 8.49
S_ilk = 120 / (14.14 * 8.49) = 120 / 120.03 = 1.00
```

**Coupling and energy:**
```
J = 0.20 * 1.00 = 0.20
E = -(0.20) * 5.0 * 3.0 = -3.00
M = 5.0 + 3.0 = 8.0
Ê = -3.00 / 64 = -0.047
```

**Scope:** weak but existing cohesion (Ê = -0.047, below -0.10 → stable scope).

---

### New message: the CTX "fútbol" comes in with a different participant

```json
{
  "ctx_003": {
    "label": "fútbol",
    "ema": 2.0,
    "tags": ["gol", "partido", "cancha"],
    "ilk_profile": {
      "ilk:tenant:juan":    { "as_sender": 3, "as_receiver": 1 },
      "ilk:tenant:pedro":   { "as_sender": 1, "as_receiver": 3 }
    }
  }
}
```

**Pairs to compute:** (001,002), (001,003), (002,003)

**(001, 003):**
```
S_tema: intersection={} → 0/6 = 0.00
J = 0 (S_ilk does not matter, the product is 0)
E_13 = 0
```

**(002, 003):**
```
S_tema: intersection={} → 0/6 = 0.00
J = 0
E_23 = 0
```

**(001, 002):** same as before = -3.00

**Total:**
```
E_total = -3.00 + 0 + 0 = -3.00
M = 5.0 + 3.0 + 2.0 = 10.0
Ê = -3.00 / 100 = -0.030
```

**EMA (alpha=0.25):**
```
energy_ema = -0.047 + 0.25 * (-0.030 - (-0.047))
           = -0.047 + 0.25 * 0.017
           = -0.047 + 0.004
           = -0.043
```

**Evaluation:** -0.043 > -0.10 → still stable. But the energy went up (from -0.047 to -0.043). If "fútbol" keeps growing and "facturación"/"pagos" decay, the energy will keep rising toward 0.

---

### After several messages: fútbol dominates

```
ctx_001 (facturación): ema = 1.0  (decayed)
ctx_002 (pagos):       ema = 0.5  (decayed)
ctx_003 (fútbol):      ema = 6.0  (grew)
```

**Energy:**
```
E_12 = -(0.20) * 1.0 * 0.5 = -0.10
E_13 = 0  (no common tags)
E_23 = 0  (no common tags)
E_total = -0.10
M = 1.0 + 0.5 + 6.0 = 7.5
Ê = -0.10 / 56.25 = -0.002
```

**EMA:**
```
energy_ema = -0.043 + 0.25 * (-0.002 - (-0.043))
           = -0.043 + 0.25 * 0.041
           = -0.043 + 0.010
           = -0.033
```

It keeps rising. After a few more messages with the same trend, `energy_ema` crosses -0.10 → **the scope changed**.

---

## 7. Constants (initial values for calibration)

| Constant | Value | What it controls |
|-----------|-------|-------------|
| `ALPHA` | `0.25` | Smoothing of the energy EMA (lower = more inertia) |
| `UNBIND_THRESHOLD` | `-0.10` | Scope change threshold (closer to 0 = more sensitive) |
| `MIN_CTX_FOR_EVAL` | `2` | Minimum number of active CTX to evaluate the scope |

These values are calibrated with real lab data. Start with these and adjust according to the observed results.

---

## 8. Integration in the current pipeline

```
Message comes in
  → Tagger (tags + ilk_sender + ilk_receiver)      // MODIFIED: add ILKs
  → Evaluator (CTX updated with weight)              // EXISTING
  → Update ilk_profile in each touched CTX            // NEW
  → Compute the normalized energy of the scope        // NEW
  → Update the energy_ema of the scope                // NEW
  → If energy_ema > UNBIND_THRESHOLD → new scope     // NEW
  → Memories and episodes (no changes)
```

Changes to the existing code:
1. **Tagger:** add `ilk_sender` and `ilk_receiver` to each tag
2. **CTX:** add the `ilk_profile` field and update it in `processMessage`
3. **Scope:** a new entity with `energy_ema`, computed post-CTX
4. **New module:** `src/scope/binding.ts` with the functions of this doc
