# jsr-client (legacy)

`jsr-client` is **deprecated** and kept only for migration compatibility.

## Use this instead

Use `fluxbee-sdk` (`crates/fluxbee_sdk`) for all new code.

```toml
[dependencies]
fluxbee-sdk = { path = "../fluxbee_sdk" }
```

## Migration notes

- New communication features and fixes should land in `fluxbee_sdk`.
- Blob contract/tooling lives in `fluxbee_sdk::blob`.
- `jsr-client` will be removed after migration tasks `M13-M14` in:
  - `docs/onworking/sdk_tasks.md`
