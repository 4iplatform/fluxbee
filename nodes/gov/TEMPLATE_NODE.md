# Template: Nuevo nodo `.gov`

Usar este template para crear un nuevo runtime de dominio `.gov` dentro de `nodes/gov/`.

## 1) Crear crate

Ejemplo para `AI.compliance.gov`:

```text
nodes/gov/ai-compliance-gov/
├── Cargo.toml
└── src/main.rs
```

`Cargo.toml` mínimo:

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

## 2) Registrar en workspace

Agregar en `Cargo.toml` raíz:

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

## 4) Config de ruteo (si aplica)

Para frontdesk de identity:

```yaml
government:
  identity_frontdesk: "AI.frontdesk.gov@motherbee"
```

## 5) Validación mínima

```bash
cargo check -p ai-compliance-gov
```

Si interactúa con identity:
- usar helpers `fluxbee_sdk::identity::*`,
- cubrir flujo en scripts E2E existentes o agregar wrapper específico en `scripts/`.
