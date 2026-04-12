# fluxbee-go-sdk

Go SDK for Fluxbee node/runtime integration.

Current v1 scope:

- Fluxbee message envelope types
- socket framing and router lifecycle
- node handshake helpers
- node config control-plane helpers
- default node status helpers
- local peer UUID -> L2 resolution through the SHM of the router that actually accepted the node connection (`ANNOUNCE.router_name`)
- direct `system` request/reply RPC helpers
- canonical `HELP` descriptor types
- base `TIMER_RESPONSE` parsing helpers
- typed `SY.timer` client surface for time operations, scheduling, reads, cancel/reschedule and `TIMER_FIRED` parsing

First-party consumers:

- `SY.opa.rules`
- `SY.timer`

## Versioning and compatibility policy

Current compatibility target:

- `fluxbee-go-sdk` is a **v1 in-repo SDK**
- it is intended for first-party nodes now and third-party Go consumers next
- wire compatibility is anchored to the current Fluxbee runtime contract and guarded by SDK tests/fixtures

Rules for v1:

- patch-level changes must be backward compatible bug fixes
- additive exported APIs are allowed in v1 as long as they do not break existing callers
- breaking wire-contract changes require:
  - explicit spec update
  - fixture/golden test updates
  - coordinated runtime + SDK migration
- breaking exported-API changes should be treated as a new major SDK phase, not as a casual refactor

Practical meaning:

- `SY.opa.rules` and `SY.timer` are the current compatibility anchors
- new first-party consumers should build on the documented surface below, not on incidental helpers

## Multiple routers per hive

The Go SDK now assumes the same runtime model that the Rust router implements:

- a node may connect to **any local router** in the hive
- the effective router is the one announced back in `ANNOUNCE.router_name`
- requester identity resolution for local `uuid -> L2` uses the SHM of that effective router
- router SHM is **per-router**, not a hive-wide merged node table

Implications for consumers:

- do not infer identity from `gateway_name`
- do not assume one router per hive
- do not assume reading one router SHM gives a merged view of the whole hive

This is sufficient for `SY.timer` because it is one-per-hive and only needs requester identity for the router it is actually attached to.

## Stable public surface for v1

The v1 stable surface is:

- node connection/lifecycle:
  - `Connect`
  - `ConnectWithClientConfig`
  - `NodeConfig`
  - `NodeSender`
  - `NodeReceiver`
  - `NodeUuidMode`
- envelope and protocol types:
  - `Message`
  - `Routing`
  - `Destination`
  - `Meta`
- request/reply helpers:
  - `BuildSystemRequest`
  - `BuildSystemResponse`
  - `RequestSystemRPC`
  - `ParseSystemResponse`
  - `ParseSystemResponseError`
- node control-plane helpers:
  - `CONFIG_GET`
  - `CONFIG_SET`
  - `CONFIG_RESPONSE`
  - default node status helpers
- identity resolution behavior used by `SY.timer`
- `HELP` descriptor types/helpers
- `SY.timer` client/types/helpers

Everything else should be treated as implementation support unless it is later documented here as part of the stable surface.
