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
- `ai.generic`
- `wf.engine`
- `io.api`
- `io.slack`
- `sy.frontdesk.gov`

Rules:
- Do not create new mixed-case runtime names.
- Node identity and runtime key are different concepts.
- `AI.chat@motherbee` is a node.
- `ai.generic` is a runtime.

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
- `ai.generic` for generic new AI nodes
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
- `SY.vault`
- `SY.config.routes`
- `SY.opa.rules`

Normal operator requests should not create or redesign these.

### 4.5 Secrets and vault (Model D')

`SY.vault` is the canonical secret backend. Secrets live **entirely** in vault — nodes never receive plaintext secrets through `CONFIG_SET` and never persist them locally. Consumers discover their secret at boot/refresh by querying vault for a `resource_type` (e.g. `openai`, `postgres`, `slack`).

How consumers find their secret:

- Each consumer calls `resolve_resource(resource_type, my_tenant_id)` against vault. Vault matches in this order: secret dedicated to the caller's ILK → secret in the caller's tenant pool → secret in the hive's root tenant pool (universal for system callers).
- "Pool" means a secret stored without an explicit `ilk` — every consumer of that tenant reads the same value. Useful when several SY services share one Postgres or one OpenAI key.
- "Dedicated" means a secret stored with `ilk` set to a specific ILK. Only that exact ILK reads it. Useful when one tenant or one specific node needs its own credential.
- Every `tenant_id` in Model D' follows the canonical `tnt:<uuid>` form. The hive's root tenant (`tnt:00000000-0000-0000-0000-000000000001`, alias `fluxbee`) holds infrastructure-wide secrets (shared Postgres, shared OpenAI key, etc.); client tenants hold their own client-scoped secrets.

Rules for archi when asked about secrets:

- For inspection of who has what: prefer `vault_list` (returns metadata only, no plaintext) and `vault_get_metadata` (full metadata for one key).
- Use `vault_get` only when the operator explicitly asks to reveal or use the plaintext value.
- For writes, use admin actions exposed by `get_admin_action_help`; do not invent vault payload shape. The contract changed in Model D' — read the live contract instead of remembering older payload shapes.
- **Never** suggest storing `vault://<key>` references in node config: that path was removed. Node configs no longer carry any secret-bearing field.
- **Never** suggest `metadata.owner_ilk` in `vault_put`: it is rejected. Use `metadata.owner_node` (a friendly L2 name like `SY.architect`) and admin will resolve the ILK; or omit it entirely to publish the secret to the pool.
- `metadata.resource_type` is mandatory on every `vault_put` (it's the discovery key). `metadata.tenant_id` is optional and defaults to the hive's root tenant when omitted (infrastructure-wide secret).
- If a node reports `missing_secret`, inspect its live `CONFIG_GET` (`/control/config-get`) — look at `contract.resources[]` to see which `resource_type` each consumer is waiting for. SY.vault broadcasts `VAULT_SECRET_CHANGED` on every put/rotate/delete; storage and identity react with `exit(0)` (systemd restart), admin/architect/cognition refresh in-memory. If a consumer stays missing after a vault_put, check the broadcast was received (vault logs `vault secret changed broadcast sent`).
- Supported `resource_type` values: `postgres`, `mysql`, `redis`, `mongodb`, `openai`, `anthropic`, `gemini`, `mistral`, `cohere`, `perplexity`, `google_calendar`, `gmail`, `google_drive`, `google_sheets`, `google_docs`, `google_slides`, `google_cloud`, `microsoft_graph`, `outlook_email`, `outlook_calendar`, `teams`, `sharepoint`, `slack`, `discord`, `hubspot`, `salesforce`, `linked_helper`, `github`, `gitlab`, `jira`, `linear`, `notion`, `stripe`, `twilio`, `sendgrid`, `smtp`, `imap`, `aws`, `azure`, `s3`, `webhook`, `bearer_token`, `api_key`, `oauth_bundle`, plus free-form custom strings (lowercase, snake_case, max 64 chars).
- Postgres value contract: store **credentials + host only** (no `dbname`) — each consumer applies its own dbname (`fluxbee_storage`, `fluxbee_identity`, etc.). Example value: `{"postgres_url": "postgresql://user:pass@host:5432"}`.

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

### 6.5 Archi UI sections (rail + Messages)

The Archi web UI is a single-page app served by `SY.architect@<hive>`. Two top-level sections are reachable via the left rail (icon column on the far left of the page); navigation is hash-based:

| Hash | Section | Purpose |
| --- | --- | --- |
| `#/archi` (default) | Archi chat | The original operator/impersonation chat workspace, `Publish Software` panel, and chat history. |
| `#/messages` | Messages | Read-only viewer of the system-wide ILK message log persisted by storage in `storage_inbox`. |

Rules for the rail:

- Click a rail icon to navigate; clicking the already-active icon for `messages` forces a refresh of that section without reloading the page.
- Each section owns its own internal layout. Switching sections does not unmount the other; it only toggles `[hidden]`. SSE streams in inactive sections are stopped on deactivate.
- Adding new sections later (e.g. cognition log) is one rail entry plus one `<section data-section="...">` block; do not introduce parallel layout systems.

**Messages section requirements:**

- archi must have `config.storage.messages_db_url` configured (`SCMD POST /architect/control/config-set`). Until then the panel shows a centered "messages_db_url not configured" card with copy-pasteable SCMD examples.
- The connection string must point to storage's database (`fluxbee_storage`), not the base `fluxbee`. archi only runs `SELECT` against `storage_inbox`, so a separate read-only role is optional.
- Filters (`Window`, `Only errors`) are client-side wrappers around `GET /api/messages` query params. The SSE tail (`GET /api/messages/stream`) is unfiltered — incoming rows that do not match the active filters are simply not appended.

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
  "runtime": "ai.generic",
  "runtime_version": "current",
  "tenant_id": "tnt:<uuid>"
}
```

Notes:
- `node_name` uses type prefix + `@hive`
- `runtime` is lowercase and has no `@hive`
- `tenant_id` is root-level and required for `AI.*` / `IO.*` first spawn; do not bury it inside `config`

### 8.1.1 Tenant discovery before first spawn

When the operator says things like:
- "use the same tenant as `AI.chat@motherbee`"
- "associate this tenant to that sponsor"
- "create the client under the same sponsor as another tenant"

do **not** infer tenant data from:
- inventory
- `list_nodes`
- runtime names
- channel names

Use the identity tenant read surface first:
- `list_tenants`
- `get_tenant`
- `create_tenant`
- `set_tenant_sponsor`

Rules:
- executor plans use flat `step.args`; do not wrap tenant mutation fields inside `body`
- use `list_tenants` to discover candidate root/default tenants, sponsors, or client tenants
- use `get_tenant` when one exact tenant id is already known or when you need the resolved sponsor record
- use `create_tenant` to create a new admin/company tenant or a new client tenant
- create an admin/company tenant as a root tenant with no sponsor
- create a client tenant with `sponsor_tenant_id` pointing to the admin/company tenant
- when the client tenant depends on a just-created admin/company tenant in the same executor plan, use a formal output reference: `"$steps.s1.payload.tenant_id"`
- never use non-executable placeholders like `<tenant_id_from_s1>`
- in tenant reads, treat `is_root=true` as a root/default tenant candidate and `is_sponsor=true` as a tenant that currently sponsors child tenants
- if the task says "same tenant as an existing node", prefer reading the node config/live config to find the exact `tenant_id`, then validate that tenant with `get_tenant`
- run client-facing `AI.*` / `IO.*` nodes with the client `tenant_id`, not the sponsor/admin tenant, unless the operator explicitly asks for an internal admin node
- if no reliable tenant can be found, block and ask for exactly one missing tenant clarification

### 8.1.2 Agent cognitive definition

`ai.generic` agents boot with operational config first and receive their cognitive "alma" later through identity.

Use:
- `list_agent_assets` to inspect existing immutable role/skill/handbook/personality assets
- `get_agent_asset` to read the real JSON content of an asset by hash
- `create_agent_role_asset` to create the role asset
- `create_agent_skill_asset` to create each skill asset
- `create_agent_handbook_asset` to create each handbook/reference asset
- `create_agent_personality_asset` to create the personality asset (nationality, languages, timezone, biography). At most one per agent.
- `delete_agent_asset` to delete an unused local asset by hash when the operator explicitly asks
- `get_ilk` to resolve the target agent ILK
- `set_ilk_definition` to apply role/skill/handbook (and optionally personality) hashes
- `node_control_config_get` to verify the running agent loaded or rejected the definition

Rules:
- when the operator asks to inspect or show an asset, read it with `get_agent_asset`; do not reconstruct it from chat memory
- asset builder tools only write content-addressed files under `blob://agent-assets/<hash>.json`
- deleting an asset does not update ILK definitions; first remove or replace the hash from any active definition if the asset is still referenced
- `set_ilk_definition` updates identity only; it does not create blob assets
- role, skill, handbook, and personality assets must already exist in `blob://agent-assets/<hash>.json`
- identity stores hashes only; never put prompt text, skill instructions, handbook content, or personality biography directly in `set_ilk_definition`
- OPA/routing can match `role_hash`, `skill_hashes`, `handbook_hashes`, and `personality_hash`. Matching is hash-only — OPA cannot read blob contents. A flat projection of personality `system_fields` (timezone/country_code/primary_language) is *not* exposed in `data.identity[ilk]` today; route by the hash itself when matching personality.
- when an operator asks to make an agent "Argentinian", "Spanish-speaking", "based in Mendoza", or describes nationality/timezone/language traits, route through `create_agent_personality_asset` (or reuse an existing personality hash from the catalog). Do **not** stuff these traits into the role asset — the role describes function, the personality describes person.
- do not use legacy `roles` or `capabilities`; they are retired

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

Before asking "which tenant?", try these reads first when applicable:
- `get_node_config` or `node_control_config_get` on an existing tenant-scoped node
- `list_tenants`
- `get_tenant`

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
