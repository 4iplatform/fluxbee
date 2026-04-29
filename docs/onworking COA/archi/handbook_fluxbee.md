# Fluxbee Pipeline Handbook

**Audience:** internal pipeline agents used by Archi: Designer, RealProgrammer, PlanCompiler, and Archi as coordinator  
**Status:** v3  
**Date:** 2026-04-29

---

## 1. Quick Decision Guide

Use this section first. If the task is clear here, do not overcomplicate it.

### 1.1 Which tool family should solve the problem?

| Situation | Correct path |
| --- | --- |
| The operator wants to inspect state, logs, inventory, runtimes, routes, workflows, or node status | `fluxbee_system_get` / read-only tools |
| The operator wants a clear mutation on existing platform primitives | `fluxbee_plan_compiler` |
| The operator wants broad desired-state design across topology, runtimes, nodes, routing, workflow deployment, or OPA deployment | `fluxbee_start_pipeline` |

### 1.2 Workflow vs routing vs OPA

| Need | Prefer |
| --- | --- |
| Deterministic choreography between known nodes | `wf_deployments` / `wf_rules_compile_apply` |
| Prefix- or hive-based forwarding between known destinations | `routing` / `add_route` |
| Policy-based target resolution when destination is not explicit | `opa_deployments` / OPA |

Rules:
- If the operator already names the participating nodes, do **not** default to OPA.
- If the operator wants fan-out, mirror, relay, echo, or ordered business flow, think **workflow first**, then routing.
- Use OPA when the business wants policy-driven target selection, not when the path is already explicit.

### 1.3 IO, AI, WF, SY boundaries

| Prefix | Role | What it is for | What it is not for |
| --- | --- | --- | --- |
| `AI.*` | conversational / reasoning nodes | language behavior, agent work, interpretation | external integrations |
| `WF.*` | orchestration / deterministic process | workflow steps, branching, stateful orchestration | generic chat nodes |
| `IO.*` | integration boundary | HTTP, Slack, WhatsApp, email, external ingress/egress | default internal relay/router |
| `SY.*` | infrastructure | admin, storage, routes config, identity, policy plumbing | normal application-level business nodes |

Rules:
- Do not invent a new `IO.*` node as an internal relay unless the operator explicitly wants a dedicated integration/relay runtime.
- Do not propose creating or modifying `SY.*` nodes for ordinary operator work.
- For internal message choreography, prefer `WF.*` or routing before proposing new `IO.*` or `SY.*`.

### 1.4 Mutation path rule

Archi does not mutate the system directly.

- For mutations, Archi should end at `fluxbee_plan_compiler` or `fluxbee_start_pipeline`.
- The emitted executor plan is what eventually calls admin actions.
- SCMD is user-directed system control and is not part of the pipeline reasoning model.

---

## 2. Core Mental Model

Fluxbee is a distributed node platform. Each **hive** is a host machine. Each hive runs one or more managed **nodes** created from published **runtimes**.

```text
human / client
      |
      v
  [IO node] <-> [AI node] <-> [WF node]
                                  |
                              [SY infra]
```

### 2.1 Runtime vs node

| Concept | Meaning | Analogy |
| --- | --- | --- |
| Runtime | published package/template | Docker image |
| Node | managed instance created from a runtime | running container |

Rules:
- Publishing a runtime does not create a node.
- A node can exist as a persisted managed instance even if the process is stopped.
- A live process is not the same thing as a persisted managed instance.

### 2.2 Naming

#### Node names

Pattern: `TYPE.name@hive`

Examples:
- `AI.chat@motherbee`
- `WF.invoice@motherbee`
- `IO.api.support@motherbee`
- `SY.admin@motherbee`

#### Runtime names

Runtime/package names are **lowercase** and do **not** include `@hive`.

Examples:
- `ai.common`
- `wf.engine`
- `io.api`
- `io.slack`
- `sy.frontdesk.gov`

Rules:
- Do not create new mixed-case runtime names.
- Node identity and runtime key are different concepts.
- `AI.chat@motherbee` is a node.
- `ai.common` is a runtime.

---

## 3. Placement, Hive, and Scope

### 3.1 How to choose a hive

Resolve hive in this order:
1. If the operator names a hive explicitly, use it.
2. If the operator names an existing node with `@hive`, use that hive.
3. If the system has only one hive, use that hive without asking.
4. If several hives exist and the target is genuinely ambiguous, ask one concise clarification.

### 3.2 When to use `motherbee` vs `worker-*`

| Situation | Prefer |
| --- | --- |
| Shared general-purpose node | `motherbee` |
| Heavy compute or special hardware | `worker-*` |
| Tenant isolation by host | dedicated `worker-*` |

If a worker hive does not exist yet, include topology/VPN creation in desired state. If it already exists, do not recreate topology.

---

## 4. Node Family Guidance

### 4.1 AI nodes

Use when the work is conversational, interpretive, or agentic.

Common runtime bases:
- `ai.common` for generic new AI nodes
- `ai.chat` is an existing chat runtime, not the default base for cloning new AI behavior

### 4.2 WF nodes and workflows

Use when the behavior is deterministic, step-based, branched, or must coordinate several nodes.

Prefer:
- `wf.engine` as runtime base for workflow-style nodes
- `wf_deployments` / `wf_rules_compile_apply` when the operator is really asking for business choreography, fan-out, ordered delivery, retry semantics, or dispatch logic

### 4.3 IO nodes

Use for external ingress/egress only:
- HTTP/API
- Slack
- WhatsApp
- email
- webhooks

Do not assume an `IO.*` node should do:
- internal routing
- workflow branching
- policy resolution
- generic relay of business traffic

### 4.4 SY nodes

These are system infrastructure:
- `SY.admin`
- `SY.storage`
- `SY.identity`
- `SY.cognition`
- `SY.config.routes`
- `SY.opa.rules`

Normal operator requests should not create or redesign these.

---

## 5. Choosing Between Routing, Workflow, and OPA

This is the section Archi was missing most often.

### 5.1 Use routing when

- the destination rule is simple and deterministic
- forwarding is based on prefix or next-hop hive
- you are connecting already-known traffic patterns between nodes/hives

Examples:
- forward `AI.specialist.*` traffic from `motherbee` to `worker-220`
- send a prefix to another hive

### 5.2 Use workflow when

- a message should trigger several downstream effects
- one inbound interaction should fan out to multiple nodes
- ordering matters
- one node response should trigger another side effect
- the operator is describing a business flow, not just a network path

Examples:
- inbound from `IO.api.support@motherbee` goes to `AI.chat@motherbee`
- the same interaction is mirrored to `IO.slack.support@motherbee`
- AI response should also be echoed to Slack

That is usually **workflow-orchestration territory**, not OPA-first.

### 5.3 Use OPA when

- destination is policy-driven
- tenant/identity/rule evaluation determines target
- the request is about governance or decision policy rather than message choreography

Examples:
- route by tenant policy
- route by identity restriction
- route by role/capability decision when `dst` is not explicit

### 5.4 Anti-patterns

Do not:
- use OPA as a substitute for deterministic workflow orchestration
- invent a new `IO.echo` node by default when workflow/routing already model the problem
- route normal application behavior through `SY.config.routes` as if it were a business node

---

## 6. Pipeline Roles

### 6.1 Designer

Role: produce a `solution_manifest` v2 describing desired state.

Valid `desired_state` sections:
- `topology`
- `runtimes`
- `nodes`
- `routing`
- `wf_deployments`
- `opa_deployments`

Not supported here:
- `policy`
- `identity`

Ownership rules:
- `solution` means the pipeline may create/update/delete it
- `external` means the reconciler must not delete it
- omitted defaults to conservative external behavior

### 6.2 RealProgrammer

Role: materialize exactly one artifact bundle per `build_task_packet`.

Artifact kinds:
- `runtime_package`
- `workflow_definition`
- `opa_bundle`
- `config_bundle`

Important:
- `package.json.name` is lowercase
- `runtime_base` must exist in known context
- `workflow_definition` is for workflow logic, not generic runtime packaging

### 6.3 PlanCompiler

Role: translate a `delta_report` into a static `executor_plan`.

Rules:
- all step args are static
- reads happen during plan generation via `query_hive`
- call `get_admin_action_help` for each action used
- do not invent actions outside compiler-class mappings

### 6.4 Archi

Role: coordinate, choose the right path, ask at most one necessary clarification, and present the result.

Archi should:
- use reads for inspection
- use `fluxbee_plan_compiler` for clear mutations
- use `fluxbee_start_pipeline` for broad desired-state design
- not mutate directly

---

## 7. Common Mutation Patterns

### 7.1 New node from existing runtime

Use when runtime already exists and is materialized.

Pre-read:
- `query_hive(list_runtimes)`
- optionally `query_hive(list_nodes)` or node status

Then:
- `run_node`

### 7.2 Publish runtime then create node

Use when runtime does not yet exist or is not materialized.

Then:
1. `publish_runtime_package`
2. `run_node`

### 7.3 Update node config

Use:
- `node_control_config_get` first when contract/version matters
- then `node_control_config_set`
- then restart only if required

Restart bias:
- `AI.*` and `IO.*`: usually hot apply
- `WF.*` and `SY.*`: usually restart unless contract says otherwise

### 7.4 Routing change

Use:
- `add_route`
- `delete_route`
- `delete_route` then `add_route` for replace

### 7.5 Workflow deployment

Use:
- `wf_rules_compile_apply`

Use this when the operator is describing:
- branching
- fan-out
- echo/mirror
- chained side effects
- deterministic multi-node behavior

### 7.6 OPA deployment

Use:
- `opa_compile_apply`

Use this only for policy-driven routing/enforcement needs.

---

## 8. Important Args and Conventions

### 8.1 `run_node`

Canonical required shape:

```json
{
  "hive": "motherbee",
  "node_name": "AI.coa@motherbee",
  "runtime": "ai.common",
  "runtime_version": "current"
}
```

Notes:
- `node_name` uses type prefix + `@hive`
- `runtime` is lowercase and has no `@hive`
- `tenant_id` is root-level when required for first spawn; do not bury it inside `config`

### 8.2 IO tenant naming

Single-tenant IO node:
- `IO.slack@motherbee`

One IO node per tenant:
- `IO.slack.T126@motherbee`

Use the same short tenant token consistently across related nodes.

---

## 9. Compiler-Class Mapping

PlanCompiler should map delta operations like this:

| compiler_class | Required steps |
| --- | --- |
| `NODE_RUN_MISSING` | `run_node` |
| `NODE_CONFIG_APPLY_HOT` | `node_control_config_set` |
| `NODE_CONFIG_APPLY_RESTART` | `node_control_config_set` -> `restart_node` |
| `NODE_RESTART_ONLY` | `restart_node` |
| `NODE_KILL` | `kill_node` |
| `NODE_RECREATE` | `kill_node` -> `run_node` |
| `RUNTIME_PUBLISH_ONLY` | `publish_runtime_package` |
| `RUNTIME_PUBLISH_AND_DISTRIBUTE` | `publish_runtime_package` |
| `RUNTIME_DELETE_VERSION` | `remove_runtime_version` |
| `HIVE_CREATE` | `add_hive` |
| `VPN_CREATE` | `add_vpn` |
| `VPN_DELETE` | `delete_vpn` |
| `ROUTE_ADD` | `add_route` |
| `ROUTE_DELETE` | `delete_route` |
| `ROUTE_REPLACE` | `delete_route` -> `add_route` |
| `WF_DEPLOY_APPLY` | `wf_rules_compile_apply` |
| `WF_DEPLOY_RESTART` | `wf_rules_compile_apply` -> `restart_node` |
| `OPA_APPLY` | `opa_compile_apply` |

Blocked classes: do not emit executor steps for `NOOP`, `WF_REMOVE`, `OPA_REMOVE`, or `BLOCKED_*`.

---

## 10. Good Usage Rules

### 10.1 Ask less, infer more

Ask one short clarification only when a critical input cannot be inferred safely.

Examples of acceptable one-question clarifications:
- which hive?
- which tenant?
- is this policy-driven or deterministic workflow behavior?

### 10.2 Prefer existing platform primitives

Prefer:
- existing runtimes
- existing nodes
- workflow deployment
- routing

Before inventing:
- new relay nodes
- new OPA bundles
- new infra nodes

### 10.3 Distinguish clear mutation from broad design

Use `fluxbee_plan_compiler` when the operator clearly knows what should change.

Use `fluxbee_start_pipeline` when the operator is effectively asking:
- what should the topology be?
- how should this distributed behavior be designed?
- what resources are needed to realize a higher-level solution?

---

## 11. What Not to Do

- Do not treat SCMD as part of pipeline reasoning.
- Do not create new `SY.*` nodes as part of ordinary app work.
- Do not use OPA as first choice for deterministic business routing.
- Do not use `IO.*` as default internal relay nodes.
- Do not create new mixed-case runtime names.
- Do not assume `motherbee` if hive is known to be something else.
- Do not publish a runtime when the operator only asked for inspection.

---

## 12. Source References

This handbook is the concise operational layer for Archi. Broader platform detail lives in:

- [01-arquitectura.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/01-arquitectura.md)
- [04-routing.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/04-routing.md)
- [10-identity-layer3.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/10-identity-layer3.md)
- [SY_nodes_spec.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/SY_nodes_spec.md)
- [executor_manifest_pilot_spec.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/executor_manifest_pilot_spec.md)

If this handbook conflicts with live action contracts, the live action contracts from `get_admin_action_help(...)` win for request shape, and this handbook wins for high-level planning intent.
