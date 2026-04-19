# Solution Manifest — Dev Notes For Specialist-Based Archi

**Status:** dev discussion draft  
**Date:** 2026-04-19  
**Audience:** SY.architect developers, prompt engineers, executor/runtime designers  
**Related docs:** `solution-manifest-spec_1.md`, `archi-macro-tools-spec.md`, `sy-policy-beta.md`, `sy-wf-rules-spec.md`

---

## 1. Why This Dev Note Exists

This document captures a design shift that emerged while evaluating macro tools for `SY.architect`.

The earlier direction was:

- build a generic macro framework inside `SY.architect`
- teach Archi to use those macros
- hide low-level Fluxbee command complexity behind a new abstraction layer

That direction is now considered too heavy for the value it adds.

The new direction is:

- keep the **solution manifest** as the main declarative design artifact
- use **specialist AI workers** to generate and validate that manifest
- use a separate **execution specialist** to turn approved plans into real Fluxbee operations
- use **typed function calls declared in code** for execution, instead of a generic macro language

The key idea is simple:

- we do **not** want another programmable mini-language inside Archi
- we **do** want a specialist that knows Fluxbee operational contracts in depth
- we **do** want typed execution functions so the model stops improvising JSONs and command sequences

---

## 2. Core Architectural Change

### 2.1 Old thought: macro framework

The macro idea tried to solve:

- "the model knows the intent but not the exact command shape"

The cost of that path was growing into:

- a macro trait
- a registry
- parameter schemas
- staged preview DSL
- compound action engine
- progress handling
- confirmation semantics
- another layer of behavior that partly duplicated admin/actions

That is too much infrastructure for what is fundamentally an execution guidance problem.

### 2.2 New thought: specialist architecture

The new model is:

- **Host Archi**
  - talks to the operator
  - coordinates specialist workers
  - presents the result
  - asks for final approval

- **Design specialists**
  - infrastructure specialist
  - workflow specialist
  - policy / OPA specialist
  - possibly AI/runtime specialist
  - possibly more than one programmer-class specialist when the solution is large

- **Auditor**
  - reviews the manifest and the plan
  - scores or blocks weak solutions
  - identifies missing constraints, unsafe assumptions, and inconsistencies

- **Execution specialist**
  - does not invent architecture
  - does not improvise payloads
  - receives an approved plan / manifest delta
  - calls typed Fluxbee execution functions declared in code
  - knows exact request shapes and sequences

This is not "3 nodes only". It is a **host + specialist pool + auditor + executor** model.

The original triunvirate idea remains valid conceptually, but now it becomes:

- one host
- one auditor
- one or more design specialists
- one execution specialist

---

## 3. The Manifest Still Matters

The move away from macros does **not** weaken the manifest. It makes it more important.

The manifest remains the right artifact because it is:

- declarative
- durable
- auditable
- diffable
- versionable
- rollbackable
- readable by humans and LLMs

Macros were trying to improve command execution.
The manifest solves a different and larger problem:

- capture the desired solution state in a stable form

That is why the manifest should stay as the center of the long-term architecture.

---

## 4. Revised Responsibility Split

### 4.1 Design layer

The design specialists produce or refine a manifest.

Their job:

- topology
- runtimes
- node set
- WF definitions and placements
- OPA/policy structure
- routing
- tenant and identity expectations

They should think in terms of:

- desired state
- constraints
- dependencies
- safety

They should **not** think in terms of:

- exact admin path strings
- body field ordering
- whether the right operation is `compile_apply` or `apply`
- how to pass `tenant_id` on first spawn

That is executor territory.

### 4.2 Audit layer

The auditor validates:

- consistency
- safety
- completeness
- policy implications
- deployment realism

The auditor should be allowed to reject a manifest or require revisions.

### 4.3 Execution layer

The execution specialist takes:

- approved manifest
- current system state
- diff / reconciliation plan

and turns that into actual Fluxbee operations via **typed execution functions**.

This is the place where detailed operational knowledge belongs.

---

## 5. Why Typed Functions Beat Macros Here

The execution specialist should use functions declared in code, similar to OpenAI function/tool calling.

Example direction:

- `ensure_hive(...)`
- `ensure_vpn(...)`
- `ensure_runtime_available(...)`
- `ensure_node_spawned(...)`
- `ensure_node_config(...)`
- `ensure_route(...)`
- `ensure_workflow_deployed(...)`
- `ensure_opa_policy_applied(...)`
- `remove_node(...)`
- `remove_route(...)`

Why this is better than a generic macro framework:

1. The schema is real code, not another abstraction language.
2. The execution specialist sees exact typed contracts.
3. The model stops guessing payload shape.
4. Each function can embed Fluxbee-specific quirks and safety rules.
5. The host does not need a huge prompt full of path trivia.
6. The system stays closer to existing commands and contracts.

This is the crucial shift:

- we are **not** creating a new language
- we are creating a **typed execution surface**

That is a much better investment.

---

## 6. Execution Specialist Requirements

The executor specialist needs very high specialization.

This worker must know:

- Fluxbee node model
- hives vs nodes vs runtimes
- admin surface
- orchestrator behavior
- first spawn vs update semantics
- config replace vs merge semantics
- OPA apply behavior
- workflow compile/apply/rollback behavior
- tenant requirements
- confirmation boundaries
- destructive vs non-destructive operations
- failure modes and partial-state consequences

This is exactly the place where deep prompt specialization is justified.

The execution specialist should be the one AI worker in the architecture that knows:

- exact JSON body contracts
- exact function parameter meanings
- exact order of safe execution
- exact interpretation of execution errors

In other words:

- the programmer specialists design
- the executor specialist operates

That separation is healthy.

---

## 7. Recommended End-State For SY.architect

### 7.1 Host

`SY.architect` host is responsible for:

- conversation with operator
- coordination of specialists
- final human confirmation
- tracking history of manifests, plans, and executions

### 7.2 Specialist set

Suggested minimum specialist roles:

- `infra_designer`
- `wf_designer`
- `policy_designer`
- `auditor`
- `infra_executor`

Possible future additions:

- `ai_runtime_designer`
- `io_designer`
- `identity_designer`
- multiple programmers with scoped ownership on large manifests

### 7.3 Executor as first-class component

The executor should not be treated as "just another programmer".

It is a different kind of specialist:

- deterministic-minded
- operational
- contract-focused
- less creative
- more exact

Its job is not to invent the system.
Its job is to execute the already approved intent safely and correctly.

---

## 8. How The New Flow Should Work

### 8.1 High-level flow

```text
Operator request
  -> Host Archi
  -> Design specialists build / revise manifest
  -> Auditor reviews and scores
  -> Host presents solution and asks for approval
  -> Host requests current system state
  -> Diff / reconciliation plan is produced
  -> Execution specialist receives the plan
  -> Execution specialist calls typed Fluxbee functions
  -> Host reports progress and outcome
```

### 8.2 Important separation

The execution specialist should receive:

- explicit desired state
- explicit current state
- explicit change set or execution plan

It should **not** have to derive architecture from vague conversation text.

That keeps execution deterministic and reduces operator surprise.

---

## 9. Implications For The Manifest Spec

The canonical manifest spec remains the right foundation, but the following clarifications are now more important.

### 9.1 Separate desired state from advisory material

The manifest currently mixes:

- state that should be reconciled
- recommendations / sizing guidance

The executor should only act on the first.

Recommended split:

- `desired_state`
- `advisory` or `design_notes`

Examples of advisory:

- estimated load
- recommendations
- narrative sizing notes

Examples of desired state:

- hives
- vpns
- runtimes required
- node instances
- routes
- policy matrix
- identity vocabulary and initial ILKs

### 9.2 Distinguish owned vs referenced resources

The executor needs to know what this solution owns.

Recommended explicit ownership semantics for manifest blocks:

- `system`
- `solution`
- `external`

Examples:

- immutable system policy pack = `system`
- tenant solution routes = `solution`
- pre-existing shared hive infrastructure = `external`

Without this, reconciliation may try to "fix" things it should only reference.

### 9.3 Make workflow and policy execution paths explicit

The executor should not infer how `WF` and `OPA` are applied.

The manifest should align with actual system ownership:

- workflow source is owned by `SY.wf-rules`
- OPA policy apply is owned by `SY.opa-rules`

The executor then calls the right typed functions instead of inventing an ad hoc path.

### 9.4 Node config semantics need explicit mode

For node config changes, the executor must know:

- replace
- merge
- preserve operational fields

This should be explicit somewhere in the manifest or in the typed execution plan.

---

## 10. What Should Not Be Built Now

Do **not** build now:

- a general macro runtime
- a macro trait/plugin platform
- another DSL for compound operations
- prompt-heavy per-tool documentation strategy

Those solve the wrong problem at the wrong level.

The right immediate investment is:

- better specialist decomposition
- typed execution functions
- better executor prompt
- better mapping from manifest diff to execution plan

---

## 11. Proposed Immediate Next Step

The next design step should be a dedicated spec for the execution specialist.

Suggested document:

- `infra-executor-functions-spec.md`

That spec should define:

- the executor's role
- the typed function catalog
- function schemas
- safety / confirmation model
- which functions are read-only vs mutating
- which functions are primitive vs composite
- how execution results are reported

That document should replace the macro catalog as the practical execution design.

---

## 12. Bottom Line

The right long-term structure is:

- manifest as declarative target state
- multiple design specialists for domain expertise
- auditor for quality control
- a separate execution specialist with deep Fluxbee operational knowledge
- typed code-declared execution functions instead of a macro language

This preserves the main gain:

- the model no longer has to guess how Fluxbee operations are expressed

without paying the cost of inventing and maintaining another programming layer inside `SY.architect`.
