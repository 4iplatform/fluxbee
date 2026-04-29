# IO Specs (Imported from `slack-node`)

This folder contains IO-related specifications imported from `D:\repos\slack-node`, curated for this repo.

## Files

- `nodos_io_spec.md`
- `io-common.md`
- `io-api-node-spec.md`
- `io-api-http-contract-examples.md`
- `io-slack.md`
- `io-slack-tasks.md`

## Current adapters covered here

- `IO.slack`
- `IO.api`
- shared `io-common`

## Adaptation note (important)

When these docs conflict with the current Fluxbee v1.16/v1.17 direction in this repo, use these rules:

1. Identity:
- IO uses shared `io-common` pipeline: `lookup -> provision_on_miss -> forward`.
- IO must not block waiting for identity before acking inbound provider traffic.
- On provision failure/timeout, IO forwards with `meta.src_ilk = null` (degraded fallback), and Router/OPA handles onboarding.
- On provision, the IO node declares `ilk_type` (`"human"` or `"agent"`) when it can identify the external counterpart's nature. This is constructive of the IO node — set in `IdentityLookupInput.ilk_type` at the call site. When omitted, the server defaults to `"human"`. `"system"` is rejected for IO-originated provisioning.

2. Context fields:
- `meta.ctx` is conversational context key.
- `meta.context` is reserved for OPA/extra metadata.
- IO metadata belongs under `meta.context.io` only.
- Identity is carried in `meta.src_ilk`. Channel metadata remains under `meta.context.io.*`.

3. Transport:
- Production node-to-router integration in this repo uses `fluxbee_sdk` + Unix sockets.
- Any TCP/shim client in imported code is transitional and outside the active workspace/runtime path.

## Known Issue / Troubleshooting

### Identity lookup fails after restart with `EACCES`

Symptom in IO logs:
- `identity lookup error; treating as miss ... EACCES: Permission denied`
- then provision path works, but lookup keeps missing after node restart.

What this means:
- The node can provision via `SY.identity`, but cannot read identity SHM (`jsr-identity-<island>`).
- This is an environment permissions issue (SHM read access), not an IO protocol issue.

Quick checks:
- `ls -l /dev/shm/jsr-identity-<island>`
- `id` (user/group running the IO process)
- For systemd services: `systemctl show <service> -p User -p Group`

Expected in correct production setup:
- IO runtime user/service must have read access to identity SHM.
- If permissions are correct, lookup should survive process restarts (no forced reprovision loop).

Temporary debug-only workaround:
- Run IO as privileged user to validate diagnosis.

Operational fix:
- Align SHM ownership/group/ACL with the IO runtime user.
- If this cannot be guaranteed by deployment defaults, escalate to core/ops to standardize SHM permissions for `sy-identity`.
