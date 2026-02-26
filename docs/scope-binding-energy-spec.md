# Scope Detection por Energía de Binding

**Para:** Programador de cognitive-lab  
**Objetivo:** Implementar detección automática de cambio de scope basada en la cohesión entre contextos activos  
**Dependencia:** Requiere CTX con tags pesados por ILK (emisor/receptor)

---

## 1. Concepto en una frase

El scope es estable mientras los contextos activos estén "ligados" entre sí (comparten temas y participantes). Cuando los contextos divergen lo suficiente y se sostiene, el scope cambió.

---

## 2. Estructuras de datos

### 2.1 Tag (salida del tagger, modificada)

Cada tag ahora lleva metadata del mensaje que lo generó:

```json
{
  "tag": "facturación",
  "ilk_sender": "ilk:tenant:juan",
  "ilk_receiver": "ilk:agent:ai.soporte"
}
```

### 2.2 CTX (contexto semántico)

Cada contexto acumula tags y perfiles de participación:

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

`ilk_profile` se actualiza con cada mensaje que toca este CTX:
- Si el mensaje viene de `ilk_sender=X` → `ilk_profile[X].as_sender += 1`
- Si el mensaje va a `ilk_receiver=Y` → `ilk_profile[Y].as_receiver += 1`

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

## 3. Algoritmo paso a paso

Se ejecuta después de actualizar los CTX en cada mensaje.

### Paso 1: Filtrar CTX activos

Solo los que tienen `ema > 0` (vivos):

```typescript
const active = contexts.filter(c => c.ema > 0);
```

### Paso 2: Calcular similitud temática entre cada par

Para cada par `(i, j)`:

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

### Paso 3: Calcular similitud de participantes entre cada par

Convertir `ilk_profile` a vector numérico y calcular coseno:

```typescript
function ilkSimilarity(ctx_i, ctx_j): number {
  // Unión de todos los ILKs presentes en ambos CTX
  const allIlks = new Set([
    ...Object.keys(ctx_i.ilk_profile),
    ...Object.keys(ctx_j.ilk_profile)
  ]);

  // Construir vectores: por cada ILK, sumar as_sender + as_receiver
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
  return dotProduct / denom;  // Coseno: 0..1
}
```

### Paso 4: Calcular acoplamiento J entre cada par

```typescript
function coupling(ctx_i, ctx_j): number {
  return tagSimilarity(ctx_i, ctx_j) * ilkSimilarity(ctx_i, ctx_j);
}
```

Rango: `0..1`. Cero = nada en común. Uno = mismos tags y mismos participantes.

### Paso 5: Calcular energía total del scope

```typescript
function scopeEnergy(active: CTX[]): number {
  let energy = 0;
  for (let i = 0; i < active.length; i++) {
    for (let j = i + 1; j < active.length; j++) {
      const J_ij = coupling(active[i], active[j]);
      const m_i = active[i].ema;
      const m_j = active[j].ema;
      energy += -J_ij * m_i * m_j;  // negativo = ligado
    }
  }
  return energy;
}
```

### Paso 6: Normalizar

```typescript
function normalizedEnergy(active: CTX[]): number {
  const E = scopeEnergy(active);
  const M = active.reduce((sum, c) => sum + c.ema, 0);
  if (M === 0) return 0;
  return E / (M * M);
}
```

Rango aproximado: `-1..0`
- `-1` = todos los CTX fuertemente acoplados (scope muy estable)
- `0` = CTX desacoplados (scope sin cohesión)

### Paso 7: Suavizar con EMA

```typescript
const ALPHA = 0.25;

function updateEnergyEma(scope: Scope, currentEnergy: number): number {
  if (scope.energy_ema === undefined || scope.energy_ema === null) {
    return currentEnergy;  // primer cálculo
  }
  return scope.energy_ema + ALPHA * (currentEnergy - scope.energy_ema);
}
```

### Paso 8: Evaluar cambio de scope

```typescript
const UNBIND_THRESHOLD = -0.10;  // a calibrar

function shouldChangeScope(scope: Scope): boolean {
  return scope.energy_ema > UNBIND_THRESHOLD;
}
```

Si `energy_ema > -0.10` → el scope perdió cohesión → crear scope nuevo.

---

## 4. Caso especial: un solo CTX activo

Con un solo CTX no hay pares, la energía es 0 y normalizada es 0. Esto no significa "cambio de scope" — significa que el scope recién empieza o que solo queda un tema.

Regla: no evaluar cambio de scope si `active.length < 2`. Con un solo CTX, el scope se mantiene.

---

## 5. Qué pasa cuando el scope cambia

```typescript
if (active.length >= 2 && shouldChangeScope(scope)) {
  // 1. Cerrar scope actual
  scope.closed_at = now;
  scope.close_reason = "energy_unbind";
  scope.final_energy = scope.energy_ema;

  // 2. Crear scope nuevo con los CTX activos actuales
  const newScope = {
    scope_id: generateId(),
    created_at: now,
    energy_ema: null,  // se calculará en siguiente mensaje
    contexts: active.map(c => c.context_id),
    participants: extractParticipants(active)
  };
}
```

---

## 6. Ejemplo completo paso a paso

### Estado inicial: dos CTX ligados

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

**Coupling y energía:**
```
J = 0.20 * 1.00 = 0.20
E = -(0.20) * 5.0 * 3.0 = -3.00
M = 5.0 + 3.0 = 8.0
Ê = -3.00 / 64 = -0.047
```

**Scope:** cohesión débil pero existente (Ê = -0.047, por debajo de -0.10 → scope estable).

---

### Mensaje nuevo: entra CTX "fútbol" con participante distinto

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

**Pares a calcular:** (001,002), (001,003), (002,003)

**(001, 003):**
```
S_tema: intersection={} → 0/6 = 0.00
J = 0 (no importa S_ilk, el producto es 0)
E_13 = 0
```

**(002, 003):**
```
S_tema: intersection={} → 0/6 = 0.00
J = 0
E_23 = 0
```

**(001, 002):** igual que antes = -3.00

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

**Evaluación:** -0.043 > -0.10 → todavía estable. Pero la energía subió (de -0.047 a -0.043). Si "fútbol" sigue creciendo y "facturación"/"pagos" decaen, la energía seguirá subiendo hacia 0.

---

### Después de varios mensajes: fútbol domina

```
ctx_001 (facturación): ema = 1.0  (decayó)
ctx_002 (pagos):       ema = 0.5  (decayó)
ctx_003 (fútbol):      ema = 6.0  (creció)
```

**Energía:**
```
E_12 = -(0.20) * 1.0 * 0.5 = -0.10
E_13 = 0  (sin tags comunes)
E_23 = 0  (sin tags comunes)
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

Sigue subiendo. Después de unos mensajes más con la misma tendencia, `energy_ema` cruza -0.10 → **scope cambió**.

---

## 7. Constantes (valores iniciales para calibración)

| Constante | Valor | Qué controla |
|-----------|-------|-------------|
| `ALPHA` | `0.25` | Suavizado de energía EMA (más bajo = más inercia) |
| `UNBIND_THRESHOLD` | `-0.10` | Umbral de cambio de scope (más cercano a 0 = más sensible) |
| `MIN_CTX_FOR_EVAL` | `2` | Mínimo de CTX activos para evaluar scope |

Estos valores se calibran con datos reales del lab. Empezar con estos y ajustar según resultados observados.

---

## 8. Integración en el pipeline actual

```
Mensaje entra
  → Tagger (tags + ilk_sender + ilk_receiver)      // MODIFICADO: agregar ILKs
  → Evaluator (CTX actualizados con peso)            // EXISTENTE
  → Actualizar ilk_profile en cada CTX tocado         // NUEVO
  → Calcular energía normalizada del scope            // NUEVO
  → Actualizar energy_ema del scope                   // NUEVO
  → Si energy_ema > UNBIND_THRESHOLD → nuevo scope   // NUEVO
  → Memorias y episodios (sin cambios)
```

Cambios al código existente:
1. **Tagger:** agregar `ilk_sender` e `ilk_receiver` a cada tag
2. **CTX:** agregar campo `ilk_profile` y actualizarlo en `processMessage`
3. **Scope:** nueva entidad con `energy_ema`, calculada post-CTX
4. **Nuevo módulo:** `src/scope/binding.ts` con las funciones de este doc
