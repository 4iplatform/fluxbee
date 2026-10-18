# Gov Nodes Workspace

This directory groups the `.gov` domain code inside Fluxbee.

Goal:
- keep a clear split between core infrastructure and domain logic,
- let external teams develop/iterate `.gov` components without touching `src/bin`,
- share common configuration and bootstrap utilities when needed.

## Structure

```text
nodes/gov/
├── common/                  # shared utilities for gov code
└── ai-frontdesk-gov/        # SY.frontdesk.gov, a core system node
```

Scope of this phase:
- the current focus is on `ai-frontdesk-gov`;
- do not add other `.gov` components until the frontdesk integration and E2E are closed.

## Conventions

- Current canonical node: `SY.frontdesk.gov@<hive>`, a core service (`sy-frontdesk-gov.service`),
  not a runtime.
- Routing configuration for temporary ILKs:
  - `government.identity_frontdesk: "SY.frontdesk.gov@motherbee"`

Control plane and secrets:
- frontdesk takes no config through `CONFIG_SET`: its prompt is embedded and the provider/model come from
  `hive.yaml` (`ai`);
- the provider key is read from `SY.vault` by `resource_type` (root tenant); `CONFIG_SET` only
  forces a re-read of it.

## Build

From the repo root:

```bash
cargo check -p gov-common
cargo check -p sy-frontdesk-gov --bin sy-frontdesk-gov
```

## Identity integration (frontdesk)

The `SY.frontdesk.gov` node must use the SDK helpers:
- `identity_system_call_ok` for `ILK_REGISTER` and `ILK_ADD_CHANNEL`
- fallback to the primary (`SY.identity@<motherbee>`) when it gets `NOT_PRIMARY`

Operational checklist: `docs/history/onworking-coa/identity_v2_tasks.md` section `E2`.

## Status contract (FR7)

For any `.gov` component, the recommended operational contract is:
- answer `get_node_status` through the SDK (default or custom handler),
- expose a consistent `health_state` (`HEALTHY|DEGRADED|ERROR|UNKNOWN`),
- keep `extensions` only for runtime-specific metrics.

Validation from the admin API:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="SY.frontdesk.gov@$HIVE_ID"
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/status" | jq .
```

Reference:
- `docs/history/02-protocolo.md` (`NODE_STATUS_GET/_RESPONSE` messages)
- `docs/history/07-operaciones.md` (per-node status runbook)
