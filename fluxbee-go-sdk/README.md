# fluxbee-go-sdk

Go SDK for Fluxbee node/runtime integration.

Current v1 scope:

- Fluxbee message envelope types
- socket framing and router lifecycle
- node handshake helpers
- node config control-plane helpers
- default node status helpers

First-party consumers:

- `SY.opa.rules`
- upcoming `SY.timer`

The SDK is intended to be reusable by third-party Go node authors once the
module layout and versioning policy are stabilized.
