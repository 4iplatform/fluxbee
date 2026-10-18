# Template: a new `.gov` node

Use this template to create a new `.gov` domain runtime under `nodes/gov/`.

## 1) Create the crate

Example for `AI.compliance.gov`:

```text
nodes/gov/ai-compliance-gov/
├── Cargo.toml
└── src/main.rs
```

Minimal `Cargo.toml`:

```toml
[package]
name = "ai-compliance-gov"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.37", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
fluxbee-sdk = { path = "../../../crates/fluxbee_sdk" }
gov-common = { path = "../common" }
```

## 2) Register it in the workspace

Add it to the root `Cargo.toml`:

```toml
[workspace]
members = [
  # ...
  "nodes/gov/ai-compliance-gov",
]
```

## 3) Naming

- L2: `AI.compliance.gov@<hive>`
- Runtime: `ai.compliance.gov`

## 4) Routing config (if it applies)

For the identity frontdesk:

```yaml
government:
  identity_frontdesk: "SY.frontdesk.gov@motherbee"
```

## 5) Minimal validation

```bash
cargo check -p ai-compliance-gov
```

If it talks to identity:

- use the `fluxbee_sdk::identity::*` helpers;
- cover the flow in the existing E2E scripts, or add a specific wrapper in `scripts/`.
