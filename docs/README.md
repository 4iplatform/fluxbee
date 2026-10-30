# Fluxbee documentation

One page per topic, written from the code. Where a page and the code disagree, the code is right
and the page is a bug.

Start with the repository's [README](../README.md), then
[reference/architecture.md](reference/architecture.md).

## If you are…

- **building on Fluxbee Cloud:** [io-cloud-api.md](io-cloud-api.md), the whole contract.
- **an API partner sending messages through IO.api:** [io/io-api-node-spec.md](io/io-api-node-spec.md).
- **writing a node:** [reference/protocol.md](reference/protocol.md), the
  [Rust SDK](../crates/fluxbee_sdk/README.md), the [AI SDK](../crates/fluxbee_ai_sdk/README.md),
  the [Go SDK](../go/fluxbee-go-sdk/README.md) and the [AI nodes](../nodes/ai/README.md).
- **operating an installation:** [reference/operations.md](reference/operations.md) and
  [packaging-and-build.md](packaging-and-build.md).

## Reference

| Page | What it covers |
|---|---|
| [architecture](reference/architecture.md) | Hives and roles, nodes, routers, links between hives, ports |
| [protocol](reference/protocol.md) | The socket, the message envelope, system messages, RPC |
| [routing-and-policy](reference/routing-and-policy.md) | Delivery, resolve, the system and user policies, routes, VPNs, taps |
| [shared-memory](reference/shared-memory.md) | Every shared-memory region, its writer, readers and layout |
| [identity](reference/identity.md) | ILKs, tenants, channels, registration, agent definitions |
| [cognition](reference/cognition.md) | Memory per communication and the memory package |
| [storage](reference/storage.md) | The router stream into SY.storage, the message log, LLM usage records |
| [operations](reference/operations.md) | Install, join, publish, upgrade, node lifecycle, health |
| [agent-tools](reference/agent-tools.md) | The tool catalog: what an `ai.generic` agent can do, each tool's card, generated from the code |

## System nodes

One page per node: [rt-gateway](nodes/rt-gateway.md) (the router),
[SY.admin](nodes/sy-admin.md), [SY.orchestrator](nodes/sy-orchestrator.md),
[SY.architect](nodes/sy-architect.md) (Archi), [SY.identity](nodes/sy-identity.md),
[SY.vault](nodes/sy-vault.md), [SY.storage](nodes/sy-storage.md),
[SY.cognition](nodes/sy-cognition.md), [SY.config.routes](nodes/sy-config-routes.md),
[SY.opa.rules](nodes/sy-opa-rules.md), [SY.timer](nodes/sy-timer.md),
[SY.wf-rules](nodes/sy-wf-rules.md), [SY.edge](nodes/sy-edge.md),
[SY.frontdesk.gov](nodes/sy-frontdesk-gov.md). Archi's handbook, shipped with the package:
[archi/handbook_fluxbee.md](archi/handbook_fluxbee.md).

## Specifications of what is built

- **Edge and exposure:** [edge-ingress-spec-v6](edge-ingress-spec-v6.md),
  [edge-egress-nat-spec](edge-egress-nat-spec.md),
  [edge-multihop-reachability-spec-v1](edge-multihop-reachability-spec-v1.md).
- **IO nodes:** [io/](io/README.md), [io-blob-spec-v1](io-blob-spec-v1.md),
  [blob-annex-spec](blob-annex-spec.md), [io-wapp-design](io-wapp-design.md).
- **Nodes and config:** [node-config-control-plane-spec](node-config-control-plane-spec.md),
  [node-spawn-config-spec](node-spawn-config-spec.md),
  [system-inventory-spec](system-inventory-spec.md),
  [immediate-conversation-memory-spec](immediate-conversation-memory-spec.md),
  [ai-frontdesk-gov-spec](ai-frontdesk-gov-spec.md),
  [identity-v2.1-agent-definition-addendum](identity-v2.1-agent-definition-addendum.md).
- **Workflows and timers:** [spec-systemnode-wf-v1_1](spec-systemnode-wf-v1_1.md),
  [sy-wf-rules-spec](sy-wf-rules-spec.md), [wf-rules-quickstart](wf-rules-quickstart.md),
  [sy-timer](sy-timer.md). Writing a workflow is the handbook's §4.2
  ([archi/handbook_fluxbee.md](archi/handbook_fluxbee.md)).
- **Policy:** [opa-distribution](opa-distribution.md).
- **AI nodes:** [ai-nodes-deploy-runbook](ai-nodes-deploy-runbook.md).

## Design records

Decisions as they were taken, with the operator's reasons. Dated; not rewritten afterwards.

- [io-web-v2](io-web-v2.md) *(in progress)*: the edge as reverse proxy with io.web behind it, Archi
  through Cloud on its own host, artifacts served by the edge; where the track stands and what is
  withdrawn.
- [control-path-hardening-v1](control-path-hardening-v1.md): what a spoke may ask of the control
  plane: SY.admin's mesh allowlist, orchestrator actions with a direction, WAN senders bound to
  their session, SSH on the DMZ hosts, only a vouched email identifies a person (A-62, A-20, A-22,
  A-42, A-79).
- [acme-edge-certificates-v1](acme-edge-certificates-v1.md): the edge's certificate from an ACME
  CA, issued and renewed by SY.admin with TLS-ALPN-01 (O6, A-78).
- [router-stream-v1](router-stream-v1.md): the router stream, cognition per communication, LLM
  usage records (A-70).
- [ilk-tenant-model-v1](ilk-tenant-model-v1.md): the ILK as the actor, the tenant as a cost center.
- [wf-standard-path-v1](wf-standard-path-v1.md): workflows through SY.admin (A-68).
- [host-posture-and-exposure-spec-v1](host-posture-and-exposure-spec-v1.md): host posture, exposure,
  certificates.
- [software-entry-o9-analysis](software-entry-o9-analysis.md): how software enters an installation.
- [audits/](audits/): code audits and review panels.

## Future

Designs not built yet: [agent-tools-v1](agent-tools-v1.md) (what an AI agent can do, declared
once), [solution-manifest-spec_1](solution-manifest-spec_1.md),
[scope-binding-energy-spec](scope-binding-energy-spec.md),
[future_implementations/](future_implementations/).

## History

[history/](history/) keeps superseded documents as they were written (some in Spanish): the old
numbered reference (01–12, 14), the first specs of nodes now documented in [nodes/](nodes/), the
working notes of 2026 (`history/onworking-coa/`, `history/onworking-noe/`), and designs overtaken
before they were built, such as
[system_node_action_catalogs](history/system_node_action_catalogs.md) and
[policy_solution_integration_tasks](history/policy_solution_integration_tasks.md). Read them for
context only; they describe things that changed.

## The lab

The PROD lab's records live in [lab/](../lab/): the version ledger
([DEPLOYMENTS.md](../lab/DEPLOYMENTS.md)), the findings
([logbook/FINDINGS.md](../lab/logbook/FINDINGS.md)), the daily logbooks and the
[HANDBOOK](../lab/logbook/HANDBOOK.md).
