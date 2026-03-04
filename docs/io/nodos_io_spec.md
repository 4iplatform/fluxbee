# Nodos IO â€“ Specification v1.16 (Fluxbee)
> Adaptation note for `json-router`:
> This spec is imported and kept as functional baseline.
> If a section conflicts with router v1.16+ docs in this repo, follow `docs/io/README.md` precedence.

## 1. Role of IO Nodes

IO nodes are **edge adapters** between external communication channels and the Fluxbee message fabric.

They are intentionally:
- Stateless (from a business perspective)
- Non-authoritative
- Simple

---

## 2. Identity Model

IO nodes:
- Resolve identity via SHM
- Never wait on `SY.identity`
- Never emit or consume identity replies synchronously

Unknown identities are handled **by the Router layer**, not IO.

---

## 3. Identity Strategies

### Strategy: Routerâ€‘Driven Onboarding

- IO forwards messages with `src_ilk = null`
- Router + OPA redirect to onboarding workflow
- Message is re-injected once identity exists

This strategy provides:
- Durability
- Stateless IO nodes
- Maximum Router alignment

---

## 4. Context Model

IO nodes:
- Produce `ich`
- Consume `ctx_seq`
- Accept `ctx_window`
- Never persist or forward history externally

The Router is the **sole owner** of conversational history.

---

## 5. Protocol Alignment

IO nodes **MUST follow Router v1.16 protocol** exactly.

Notably:
- `meta.ctx` is the only context identifier
- `meta.context` is reserved for OPA input
- No duplication of context fields

---

## 6. Persistence Model

IO persistence exists only as an **implementation detail**.

Rules:
- Abstract interface required
- In-memory allowed for MVP
- No coupling to Router storage
- No assumptions of durability

---

## 7. Nonâ€‘Responsibilities

IO nodes explicitly do NOT:
- Store conversation history
- Perform identity orchestration
- Implement retry queues for business messages
- Decide routing or business logic

---


### AclaraciÃ³n â€“ meta.ctx vs meta.context

SegÃºn la especificaciÃ³n del Router v1.16:

- `meta.ctx` representa el **Contexto Conversacional (CTX)**
- `meta.context` se reserva exclusivamente para **datos adicionales evaluados por OPA**

`meta.context` **NO** debe usarse para almacenar historia, turns ni contexto conversacional.

