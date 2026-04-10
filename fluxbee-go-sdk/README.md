# fluxbee-go-sdk

Go SDK for Fluxbee node/runtime integration.

Current v1 scope:

- Fluxbee message envelope types
- socket framing and router lifecycle
- node handshake helpers
- node config control-plane helpers
- default node status helpers
- local peer UUID -> L2 resolution through `uuid_persistence_dir`
- direct `system` request/reply RPC helpers
- canonical `HELP` descriptor types
- base `TIMER_RESPONSE` parsing helpers

First-party consumers:

- `SY.opa.rules`
- upcoming `SY.timer`

The SDK is intended to be reusable by third-party Go node authors once the
module layout and versioning policy are stabilized.
