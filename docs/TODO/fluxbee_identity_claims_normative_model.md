# Fluxbee — Identity Claims, Recognition, Normative and Enforcement Model

**Status:** draft / future  
**Audience:** Identity, router, OPA, gov nodes, AI.frontdesk, SDK  
**Purpose:** define a simpler conceptual model for ILK identity metadata, separating:
1. what an ILK is and declares,
2. what the system considers correct,
3. what the system actually enforces.

---

## 1. Problem

The current identity direction tends to resemble a conventional software user model:
- roles,
- capabilities,
- controlled vocabulary,
- permission-like routing criteria.

That model is useful for static systems, but it is not the right conceptual center for Fluxbee.

In Fluxbee, ILKs are not just software accounts. They represent entities that can:
- think,
- speak,
- declare what they know,
- be recognized by others,
- assume responsibilities,
- act within a shared world.

The system therefore needs a way to document:
- what an ILK says it knows,
- what others recognize it knows,
- what responsibilities it is associated with,
- and, for institutional nodes, what office they occupy,

without turning identity into:
- a bureaucracy,
- a giant catalog,
- or a classic RBAC/ACL system.

---

## 2. Design Principles

### 2.1 Identity is descriptive first
Identity should describe the entity:
- who it is,
- what it declares,
- what others recognize,
- what office it occupies if institutional.

Identity is **not** the place where the system defines all rules of life.

### 2.2 Normative is external to identity
The system needs a normative layer, but that layer must remain outside `SY.identity`.

Normative defines:
- what is considered correct,
- what principles apply globally,
- what rules of work apply to specific offices or domains.

Identity may reference normative documents, but does not own them.

### 2.3 Enforcement is separate
What the system actively enforces must stay in a distinct layer:
- OPA policies,
- routing constraints,
- prompt restrictions,
- runtime operational controls.

Identity describes.  
Normative orients.  
Enforcement acts.

### 2.4 Claims, not permissions
The core unit for ILK metadata should not be a permission or role in the classic software sense.

The core unit should be a **claim**:
- a statement about knowledge,
- function,
- or responsibility.

Claims can later be used by:
- humans,
- frontdesk,
- routing logic,
- governance nodes,
- certification flows.

But they should not be born as ACL entries.

### 2.5 Recognition matters
A claim becomes more meaningful when another ILK recognizes it.

The system should model not only:
- what is declared,

but also:
- who recognizes it.

This supports a social/epistemic identity model rather than a purely administrative one.

---

## 3. The Three Layers

Fluxbee should separate identity-related concerns into three layers.

### 3.1 Identity Layer

Owned by `SY.identity`.

Stores:
- core ILK identity,
- claims,
- recognition,
- institutional role metadata for government nodes,
- references to normative documents.

This layer answers:
- who is this ILK,
- what does it declare,
- what do others recognize,
- what office does it occupy.

### 3.2 Normative Layer

External to `SY.identity`.

Stores:
- Constitution,
- Charters / regulations.

This layer answers:
- what is considered correct,
- what principles apply to everyone,
- what framework applies to institutional offices.

This layer is documentary first.

### 3.3 Enforcement Layer

Operational layer.

Includes:
- OPA,
- prompts,
- routing constraints,
- runtime controls.

This layer answers:
- what the system actually does in a live situation.

Enforcement may consume identity metadata and normative references, but it does not replace them.

---

## 4. Claims Model

### 4.1 Why claims

A flat list of tags is too weak and ambiguous.

A classic role/capability model is too restrictive and administrative.

A claim-based model is more aligned with Fluxbee:
- simpler than a full ontology,
- richer than tags,
- less bureaucratic than profiles/permissions.

### 4.2 Base claim facets

For normal ILKs, the system should support a small number of claim facets:

- `domain`
- `function`
- `responsibility`

No more is required in v0.

#### Domain
Describes what topics or areas the ILK can legitimately speak about.

Examples:
- identity_lifecycle
- industrial_maintenance
- metals_process
- customer_onboarding

#### Function
Describes what the ILK knows how to do operationally.

Examples:
- complete_human_registration
- classify_incoming_request
- diagnose_basic_failure
- escalate_to_specialist

#### Responsibility
Describes what the ILK is associated with as an active duty or expected area of handling.

Examples:
- human_onboarding
- identity_frontdesk_flow
- tenant_initial_association
- billing_escalation_intake

---

## 5. Claims and Recognition

The model should distinguish between:
- declaration,
- recognition.

### Declaration
A claim stated by the ILK itself or by its originating manifest / package.

### Recognition
A claim acknowledged by another ILK or institutional node.

This is enough for a useful v0.

No complex truth engine is required.

### Minimal structure

```json
{
  "facet": "domain",
  "key": "identity_lifecycle",
  "statement": "Knows the ILK/ICH/tenant lifecycle and can reason about identity registration.",
  "declared_by": "ilk:self",
  "recognized_by": ["ilk:government.identity"]
}
```

This is intentionally simple.

---

## 6. Avoiding a Giant Catalog

The system should avoid both extremes:

### Bad extreme A: pure free text
Too human-readable, but hard for systems to use consistently.

### Bad extreme B: giant controlled ontology
Too rigid, too bureaucratic, too expensive to maintain.

### Recommended compromise
Use:
- a short stable `key`,
- a human-readable `statement`.

Example:

```json
{
  "facet": "function",
  "key": "complete_human_registration",
  "statement": "Can guide a temporary human ILK through completion and association."
}
```

This allows:
- readability,
- machine use,
- normalization,
- evolution.

---

## 7. Role of AI.frontdesk

`AI.frontdesk` is the natural place to normalize human-facing language into Fluxbee internal identity language.

This extends its current role in identity flows.

### 7.1 External vs internal language

Externally, humans may speak in arbitrary language:
- “sé de ventas”
- “puedo atender clientes”
- “entiendo onboarding”
- “manejo reclamos”

Internally, Fluxbee should normalize those statements into stable claim keys.

Example:
- user says: “me encargo de registrar personas nuevas”
- frontdesk normalizes to:
  - `facet = responsibility`
  - `key = human_onboarding`

### 7.2 Frontdesk as normalizer, not truth authority

Frontdesk should:
- collect declarations,
- normalize wording,
- write structured claims,
- optionally attach first recognition.

Frontdesk should **not** become the ultimate authority of truth.

It is a normalizer and registrar, not the owner of reality.

---

## 8. Institutional / Government ILKs

Not all ILKs should be described the same way.

A normal ILK can be described through:
- identity,
- claims,
- recognition.

A government ILK needs additional institutional semantics.

### 8.1 Why institutional metadata is different

Government nodes are not important only because of what they know.

They matter because:
- they occupy an office,
- they act within a jurisdiction,
- they operate under a charter,
- they may be prompt-restricted and policy-routed.

Therefore, institutional ILKs need a more formal but still minimal block.

### 8.2 Minimal institutional block

For government nodes, add:

- `office`
- `jurisdiction`
- `charter_ref`

Example:

```json
{
  "institutional_role": {
    "office": "government.identity.frontdesk",
    "jurisdiction": ["identity", "interlocutor_onboarding"],
    "charter_ref": "charter://identity/frontdesk-v1"
  }
}
```

This is intentionally minimal.

Do **not** expand this into a giant administrative structure unless real operational need appears later.

---

## 9. Normative Layer

### 9.1 Purpose

The normative layer defines what is considered correct in Fluxbee.

It should remain external to `SY.identity`.

Identity can reference normative documents, but should not store or interpret the full corpus.

### 9.2 Maximum two levels

To avoid bureaucracy, the normative model should remain extremely simple.

Only two layers are needed:

#### 1. Constitution
Global principles and common rules of life.

Applies system-wide.

#### 2. Charters
Office- or domain-specific working rules.

Examples:
- identity frontdesk charter
- routing governance charter
- tenant admission charter

No more layers are required in v0.

No penal code.  
No judicial logic.  
No heavy compliance system.

### 9.3 Documentary first

Normative documents should be:
- versioned,
- readable,
- referenceable,
- simple.

They are documentary first, not executable first.

---

## 10. Reading Normative Documents

Normative should not be modeled as a bureaucratic “must read and sign” system.

### 10.1 For normal ILKs
Normative is mainly consultative:
- available on request,
- referenced when needed,
- useful for explanation and shared context.

### 10.2 For government ILKs
Normative is constitutive:
- the node is born attached to a charter,
- the charter shapes its role,
- the charter may be injected into prompt/context,
- OPA may enforce operational fragments derived from it.

So the system should not enforce “reading” in the human compliance sense.

Instead, it should enforce **contextual applicability** when necessary.

---

## 11. Library or Live Service?

The normative layer should be hybrid.

### 11.1 Source of truth
A versioned document library:
- constitution documents,
- charter documents.

### 11.2 Live resolver
A lightweight resolver that can answer:
- what constitution applies,
- what charter is linked to an office,
- what normative references apply to a given institutional role.

This resolver should not become a large “governance engine”.

Its job is only:
- reference resolution,
- retrieval,
- lightweight mapping.

---

## 12. OPA and Enforcement

OPA must stay in the enforcement layer.

It should not become:
- the meaning of identity,
- the source of truth of expertise,
- the holder of the constitution.

OPA should only enforce operational projections.

### 12.1 Good uses for OPA
- route temporary ILKs to frontdesk
- route institutional offices to specific flows
- block incompatible operational paths
- enforce routing decisions based on current policy

### 12.2 Bad uses for OPA
- storing all epistemic truth
- representing the entire constitution
- replacing claims with policy
- defining identity semantics

OPA consumes identity metadata.  
OPA does not define the full ontology of identity.

---

## 13. Recommended ILK Shape (v0)

### 13.1 Common ILK

```json
{
  "ilk_id": "ilk:...",
  "identity": {
    "kind": "node|interlocutor",
    "ilk_type": "agent|system|human",
    "registration_status": "temporary|partial|complete"
  },
  "claims": [
    {
      "facet": "domain|function|responsibility",
      "key": "identity_lifecycle",
      "statement": "Knows the ILK/ICH/tenant lifecycle.",
      "declared_by": "ilk:self",
      "recognized_by": ["ilk:government.identity"]
    }
  ]
}
```

### 13.2 Government ILK

```json
{
  "ilk_id": "ilk:...",
  "identity": {
    "kind": "node",
    "ilk_type": "system",
    "registration_status": "complete"
  },
  "institutional_role": {
    "office": "government.identity.frontdesk",
    "jurisdiction": ["identity", "interlocutor_onboarding"],
    "charter_ref": "charter://identity/frontdesk-v1"
  },
  "claims": [
    {
      "facet": "responsibility",
      "key": "human_registration",
      "statement": "Handles human registration and completion flows.",
      "declared_by": "ilk:government.identity"
    }
  ]
}
```

---

## 14. What Identity Stores vs What It Does Not Store

### 14.1 Identity should store
- ILK base identity
- identification / association core metadata
- claims
- recognition
- institutional role metadata
- normative references (`charter_ref`, possibly constitution refs if needed)

### 14.2 Identity should not own
- the constitution itself
- full charter texts
- routing policy logic
- prompt restrictions
- certification exams
- judicial / penal systems
- cognition

---

## 15. Migration Direction from Current Model

The current spec mentions:
- roles,
- capabilities,
- vocabulary,
- degree metadata,
- OPA integration.

That is useful as implementation history, but the desired conceptual direction is different.

### Recommended evolution
From:
- roles/capabilities as central identity vocabulary

Towards:
- claims/recognition as central identity vocabulary

Keep:
- OPA for enforcement
- frontdesk for normalization
- degree data as separate/future concern
- institutional office metadata for gov nodes

Replace the conceptual center:
- from “permission language”
- to “identity and recognized function language”

---

## 16. Summary

Fluxbee identity should evolve toward a simpler and more human model:

### Layer 1 — Identity
Stores:
- who an ILK is,
- what it declares,
- what others recognize,
- what office it occupies if institutional.

### Layer 2 — Normative
Defines:
- common principles (Constitution),
- office/domain work rules (Charters).

### Layer 3 — Enforcement
Executes:
- routing constraints,
- OPA policies,
- prompt restrictions,
- runtime controls.

### Core semantic unit
The core unit of identity metadata should be the **claim**, not the permission.

### Recognition
Claims gain meaning through recognition by other ILKs.

### Government nodes
Government nodes need a minimal institutional block:
- office,
- jurisdiction,
- charter_ref.

### Frontdesk
Frontdesk should normalize human language into internal claim keys and structured metadata.

---

## 17. Non-goals

This model does **not** aim to provide, in v0:
- a full ontology of knowledge,
- a complex trust or reputation system,
- legal enforcement,
- bureaucratic compliance tracking,
- a giant controlled catalog,
- a penal code,
- a judicial subsystem.

Fluxbee needs a living identity model, not a ministry.

---

## 18. Next Step

Immediate implementation direction:

1. add `claims[]` to ILK metadata,
2. support `declared_by` and `recognized_by`,
3. add `institutional_role` for government ILKs,
4. keep normative external,
5. use frontdesk as claim normalizer for human-facing flows,
6. let OPA consume only the operational subset needed for enforcement.
