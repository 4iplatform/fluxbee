# Layered designer — iteration model for SY.architect designer agent

**Date:** 2026-05-17
**Scope:** Replace single-pass `submit_solution_manifest` with structured per-layer iteration so the designer cannot silently skip a platform layer.
**Related:** `archi_pipeline_tooling_bugs_tasks.md` (ARCHI-STRAT-LAYERED), `handbook_fluxbee.md` (will gain a new section on layered model and plan ordering).

---

## 1. Problem statement

The designer agent today receives a task and produces a `solution_manifest` in one free pass. The schema validator accepts any manifest with the required top-level shape, even if `desired_state` covers only a subset of the layers the request actually touches.

Observed failure mode (2026-05-17 trace, restart_from_design):

- Iter 1: invalid output (missing `advisory`).
- Iter 2: designer did not call `submit_solution_manifest`.
- Iter 3: valid manifest with `section_count = 3` of a possible 7. Auditor reported `revise`. Pipeline hit `max_iterations`.

The designer is competent on individual layers when prompted to focus on one, but the unconstrained "produce the full manifest" task leaves room to forget a layer entirely. The current auditor catches gaps reactively, which costs tokens and iterations.

---

## 2. The platform layers

Fluxbee changes touch up to six layers. Lower layers must be satisfied before higher layers reference them.

| # | Layer | Resources | What "needs work" looks like |
|---|-------|-----------|------------------------------|
| L1 | **Platform / Infra** | hives, rt-gateway, SY.* core services, dist sync | new hive, new worker, missing SY service. Mostly out-of-scope (orchestrator territory); designer only declares it when a solution introduces a new hive |
| L2 | **Runtime distribution** | runtime packages (publish_runtime_package) | a node references a runtime that is not yet published or not yet materialized on the target hive |
| L3 | **Identity & secrets** | tenants (TNT_CREATE), ILK registrations, vault_put for resource secrets | new tenant not yet created; a node needs a secret (openai key, postgres URL) the operator hasn't loaded |
| L4 | **Nodes** | managed instances created via run_node | new node to spawn; existing node to reconfigure |
| L5 | **Routing / Transport** | add_route, VPN config, prefix bindings | new path between nodes; new cross-hive route |
| L6 | **Application logic** | wf_rules_compile_apply, opa_compile_apply, workflow definitions, OPA bundles | new workflow definition; new policy bundle; existing workflow re-compile |

A given request might touch one layer (e.g. "add a route between two existing nodes" → only L5) or many (e.g. "create a sales workflow ingesting from API, AI, mirror to Slack" → L2, L3, L4, L5, L6).

---

## 3. Iteration contract

### 3.1 New tool: `assess_layer`

Replaces the single-call `submit_solution_manifest` flow with N+1 calls.

```jsonc
{
  "name": "assess_layer",
  "description": "Record the assessment of one platform layer for the solution under design. Must be called exactly once per layer in the canonical layer order.",
  "parameters": {
    "type": "object",
    "additionalProperties": false,
    "required": ["layer", "outcome"],
    "properties": {
      "layer": {
        "type": "string",
        "enum": ["infra", "runtimes", "identity_secrets", "nodes", "routing", "logic"]
      },
      "outcome": {
        "type": "string",
        "enum": ["work", "no_work", "blocked"]
      },
      "manifest_fragment": {
        "type": "object",
        "description": "Required when outcome=work. Partial manifest section(s) the designer would contribute for this layer. Must validate against the per-layer fragment schema (see §4)."
      },
      "no_work_reason": {
        "type": "string",
        "description": "Required when outcome=no_work. Short plain-language explanation of why this layer needs nothing for the current request (e.g. 'all referenced runtimes are pre-published')."
      },
      "blocked_reason": {
        "type": "string",
        "description": "Required when outcome=blocked. Concrete missing information or constraint."
      },
      "blocked_code": {
        "type": "string",
        "description": "Required when outcome=blocked. Machine-friendly key (e.g. RUNTIME_UNKNOWN, TENANT_AMBIGUOUS, WORKFLOW_DEFINITION_MISSING)."
      }
    }
  }
}
```

### 3.2 Call sequence enforced by the host

The designer agent runs inside a `FunctionCallingRunner`. The host enforces:

1. `assess_layer` must be called for every layer in `["infra", "runtimes", "identity_secrets", "nodes", "routing", "logic"]`, in that order.
2. Calling `assess_layer` out of order is rejected with a deterministic error fed back to the model.
3. Calling `assess_layer` twice for the same layer is rejected (use `no_work` if you want to skip).
4. `submit_solution_manifest` is rejected until all six `assess_layer` calls have completed.
5. After the six `assess_layer` calls, `submit_solution_manifest` accepts the merged manifest. The host validates that the manifest is consistent with the fragments (the designer can refine in the merge step but cannot contradict per-layer assessments).

### 3.3 What "no_work" means structurally

`no_work` is **explicit silence**. It is not the same as forgetting the layer.

- Plan compiler will not emit operations for that layer.
- Reconciler/diff will not look for desired-state on that layer.
- The auditor records the reason in the verdict for operator visibility.

### 3.4 What "blocked" means structurally

`blocked` means "I have enough information to know this layer needs work, but not enough to specify it." Handling depends on Open Question 2 (§7).

---

## 4. Per-layer fragment schema

Each `assess_layer(layer, outcome=work, manifest_fragment=...)` carries only the manifest sections that belong to that layer. The host merges them into a single `desired_state` and `advisory`.

| Layer | Allowed `manifest_fragment` keys |
|-------|----------------------------------|
| infra | `desired_state.topology`, `advisory[*]` |
| runtimes | `desired_state.runtimes`, `advisory[*]` |
| identity_secrets | `desired_state.identity` (when allowed in V2.1+), `desired_state.tenants`, vault-put hints in `advisory`, `advisory[*]` |
| nodes | `desired_state.nodes`, `advisory[*]` |
| routing | `desired_state.routing`, `advisory[*]` |
| logic | `desired_state.wf_deployments`, `desired_state.opa_deployments`, `advisory[*]` |
| (any) | `solution`, `ownership` may appear in any fragment; merged by host |

`solution` and `ownership` are cross-cutting; the host expects them to be consistent across fragments and merges accordingly.

---

## 5. Auditor changes

The audit verdict already has `score`, `status`, `blocking_issues`, `findings`. We add a structural pre-check before the LLM auditor runs:

```text
for each required_layer:
    if designer did not call assess_layer(layer):
        emit blocking_issue = "LAYER_COVERAGE_INCOMPLETE"
        finding.section = format!("layer.{}", required_layer)
        skip LLM call (deterministic failure)
```

The LLM auditor then audits content quality, not coverage. This:
- saves tokens on coverage issues (deterministic check)
- gives the operator a precise layer-level error code
- frees the LLM auditor to focus on internal consistency and ownership

---

## 6. Budget approach (Open Question 1)

### Two viable models:

**Model A — single global budget, per-layer observability**
- `TaskTokenBudget` keeps a single hard cap for the entire design phase.
- Each `assess_layer` call records `tokens_used_per_layer` in the trace for observability.
- The model can spend more on one complex layer and less on a trivial one, as long as global remains under cap.
- Simpler to implement; mirrors today's `token_budget.add("design.designer", ...)`.

**Model B — per-layer hard caps**
- Each layer has its own sub-budget (e.g. 4k tokens).
- If a layer overruns its sub-budget, that layer is marked `blocked` and the loop continues.
- Prevents one layer from monopolizing the global pool.
- Harder to pick correct per-layer caps; some layers genuinely need more.

### Recommendation: **Model A**.

Reasons:
- Layer-level token usage is highly variable per solution; rigid per-layer caps will cause false blocks.
- We get the operational signal we need (per-layer trace) without artificial cliffs.
- If observability shows that one layer reliably eats most of the budget for a class of solutions, we can refine to Model B later.

### Decision (2026-05-17): **Model A**.

---

## 7. Layer failure semantics (Open Question 2)

When `assess_layer(layer, outcome=blocked, blocked_code=..., blocked_reason=...)`, three options:

**Option A — discard the whole manifest**
- Treat a single blocked layer as a fatal design failure.
- Pipeline blocks; operator sees the blocker; they retry/restart_from_design with more info.
- Aligns with "if you can't program nodes, you can't program routing on them" — strict ordering.
- Simplest. Easiest to reason about.
- Cons: a complex solution with one ambiguous layer (e.g. "which tenant for the new node?") still triggers full restart even if everything else was assessed cleanly.

**Option B — retry just the blocked layer**
- Run a per-layer iteration with its own cap (e.g. 2 attempts per layer).
- On second attempt the designer receives the previous `blocked_reason` as feedback for that layer only.
- If still blocked, escalate to manifest-level block.
- Saves tokens (don't reassess clean layers).
- Slightly more complex (loop inside loop).

**Option C — emit manifest with blocked layer marked**
- Manifest is submitted with one layer flagged `blocked`.
- Plan compiler explicitly refuses to compile while any layer is `blocked`.
- Operator sees exactly which layer needs more input, fixes it via chat, restart_from_design uses the prior context.
- Loses "fail fast" but preserves the work on the clean layers.

### Recommendation: **Option B + Option C combined**.
- Try a per-layer retry first (cheap).
- If still blocked, submit the manifest with `blocked` marker (Option C semantics).
- Plan compiler refuses to compile; operator sees a precise layer-scoped blocker.

Either of A/B/C is defensible; the user must pick before implementation proceeds.

---

## 8. Designer prompt changes (preview)

Replace the current free-form "produce solution_manifest" section with:

```text
## Iteration

You will reason about the solution one layer at a time, in this exact order:

1. infra       — hives, topology
2. runtimes    — runtime packages to publish
3. identity_secrets — tenants, ILKs, vault secrets
4. nodes       — managed node instances
5. routing     — routes and VPNs
6. logic       — workflows and OPA policies

For each layer you MUST call `assess_layer(layer, outcome, ...)` exactly once, in the order above, before calling `submit_solution_manifest`.

For each layer ask yourself:
- "Does this request require ANY change at this layer?"
- If no → call `assess_layer(layer=<this>, outcome="no_work", no_work_reason="...")`
- If yes and you can specify it → call `assess_layer(layer=<this>, outcome="work", manifest_fragment={...})`
- If yes but you cannot specify it from current info → call `assess_layer(layer=<this>, outcome="blocked", blocked_code="...", blocked_reason="...")`

Then call `submit_solution_manifest` with the merged manifest. The host will validate that your final manifest is consistent with your per-layer fragments.
```

---

## 9. Auditor prompt changes (preview)

Add to the existing auditor prompt:

```text
## Layer coverage

The designer must have produced an explicit `assess_layer` outcome for every layer (infra, runtimes, identity_secrets, nodes, routing, logic). If any layer is missing, that is a structural failure already flagged by the host as `LAYER_COVERAGE_INCOMPLETE`. You do not need to re-flag coverage; focus on the CONTENT of each layer fragment:

- `infra`: are declared hives consistent with reality?
- `runtimes`: do declared runtime packages have valid `package_source`?
- `identity_secrets`: do declared tenants exist or have a creation step? Do vault hints carry resource_type?
- `nodes`: does every node reference a runtime that is either pre_published, in the runtimes fragment, or otherwise resolvable?
- `routing`: does every route reference src/dst that exist as nodes (either in the nodes fragment or marked external)?
- `logic`: do workflow/OPA deployments reference nodes that exist?

If a `blocked` layer is present, your verdict should be `revise` with the layer's blocked_reason carried into findings.
```

---

## 10. Implementation order

1. **Doc**: this file + handbook section. **Done.**
2. **Decide Open Questions 1 and 2 with the operator.** ← blocker before code.
3. **Tool**: add `AssessLayerTool` and modify `SubmitSolutionManifestTool` to require all six `assess_layer` outcomes recorded in shared state.
4. **Designer flow**: `run_designer_with_context` accumulates per-layer outcomes; merges fragments; calls submit only when complete.
5. **Auditor**: structural `LAYER_COVERAGE_INCOMPLETE` check before LLM call; updated prompt for content-only audit.
6. **Tests**: unit tests covering: full-coverage manifest passes; missing layer call fails with deterministic error; one `no_work` layer passes; one `blocked` layer triggers the chosen failure semantics.
7. **Handbook**: add formal "Layered model and plan ordering" section.
8. **Re-run reference E2E** (the WF.sales request). Expect single design pass with all six `assess_layer` outcomes recorded.

---

## 11. What this does NOT change

- Number of pipeline agents stays the same (no specialists added).
- Schema of `SolutionManifestV2` is unchanged (no version bump). Per-layer fragments are merged into the existing shape.
- Reconciler, plan compiler, executor unchanged for now.
- Budget tracking continues to use `TaskTokenBudget` with the same global cap.

This is a structural change to the designer's *process*, not a rewrite of the pipeline.
