# IO Specs (Imported from `slack-node`)

This folder contains IO-related specifications imported from `D:\repos\slack-node`, curated for this repo.

## Files

- `nodos_io_spec.md`
- `io-common.md`
- `io-slack.md`
- `io-slack-tasks.md`

## Adaptation note (important)

When these docs conflict with the current Fluxbee v1.16/v1.17 direction in this repo, use these rules:

1. Identity:
- IO must be lookup-only (best-effort) via SHM.
- IO must not block waiting for `SY.identity`.
- On miss/unavailable, IO forwards with `meta.src_ilk = null` and Router/OPA handles onboarding.

2. Context fields:
- `meta.ctx` is conversational context key.
- `meta.context` is reserved for OPA/extra metadata.
- IO metadata belongs under `meta.context.io` only.

3. Transport:
- Production node-to-router integration in this repo uses `fluxbee_sdk` + Unix sockets.
- Any TCP/shim client in imported code is transitional and isolated under `io/legacy`.
