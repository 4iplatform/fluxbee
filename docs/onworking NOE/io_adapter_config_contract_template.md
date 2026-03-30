# IO Adapter Config Contract Template (WhatsApp / Instagram / Gmail / Calendar)

## Objetivo

Plantilla estándar para crear nuevos adapters IO usando el contrato común definido en:

- `nodes/io/common/src/io_adapter_config.rs`
- `nodes/io/common/src/io_control_plane.rs`

Esta guía evita acoplar nuevos adapters a `IO.slack` y mantiene consistencia de Control Plane.

---

## 1) Checklist mínimo por adapter

- [ ] Crear módulo `nodes/io/common/src/io_<adapter>_adapter_config.rs`.
- [ ] Implementar `IoAdapterConfigContract`.
- [ ] Definir `required_fields`, `optional_fields`, `notes`.
- [ ] Implementar `validate_and_materialize(payload.config)`.
- [ ] Implementar `redact_effective_config(...)` para secretos inline.
- [ ] Implementar `secret_descriptors(...)` con `NodeSecretDescriptor`.
- [ ] Agregar tests de:
  - validación OK/error,
  - defaults materializados,
  - redacción de secretos,
  - metadata `contract.secrets[*]`.

---

## 2) Esqueleto Rust recomendado

```rust
use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use fluxbee_sdk::node_secret::NodeSecretDescriptor;
use serde_json::Value;

pub struct IoWhatsappAdapterConfigContract;

impl IoAdapterConfigContract for IoWhatsappAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.whatsapp"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        &[
            "config.whatsapp.access_token | config.whatsapp.access_token_ref",
            "config.whatsapp.phone_number_id",
        ]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "config.io.dst_node",
            "config.identity.target",
            "config.identity.timeout_ms",
        ]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "MVP apply mode: replace",
            "Secret values must be redacted in responses/logs",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        // 1) validar shape base (objeto)
        // 2) validar campos requeridos adapter-specific
        // 3) materializar defaults operativos (ej: io.dst_node=resolve)
        // 4) devolver config efectiva normalizada
        unimplemented!()
    }

    fn redact_effective_config(&self, effective: &Value) -> Value {
        // Redactar secretos inline si existen
        effective.clone()
    }

    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        let mut token = NodeSecretDescriptor::new(
            "config.whatsapp.access_token",
            "whatsapp_access_token",
        );
        token.required = true;
        token.configured = effective.is_some(); // reemplazar por lógica real
        vec![token]
    }
}
```

---

## 3) Reglas de diseño (no negociables)

1. `payload.config` es del adapter:
- core no interpreta schema de negocio.

2. Compatibilidad Control Plane:
- `CONFIG_GET` / `CONFIG_SET` / `CONFIG_RESPONSE`,
- `apply_mode=replace` en MVP.

3. Secrets:
- nunca devolver secretos en claro en `CONFIG_RESPONSE` o `STATUS`,
- describirlos por `contract.secrets[*]`,
- redacción obligatoria de inline secrets.

4. Precedencia managed-node:
- dinámica persistida del nodo > fallback `config.json` orchestrator > `UNCONFIGURED`.

---

## 4) Test matrix mínima por adapter

- `invalid_config` cuando faltan requeridos.
- éxito con config mínima válida.
- materialización de defaults esperados.
- redacción efectiva de secretos inline.
- `contract.secrets[*].configured` refleja estado real.

---

## 5) Mapeo rápido por familia (ejemplos)

- `IO.whatsapp`: token + phone id + verify secret (según provider mode).
- `IO.instagram`: page token / app secret refs.
- `IO.gmail`: OAuth refresh token ref + mailbox identity.
- `IO.calendar`: OAuth scopes + calendar id + credential ref.

Cada uno conserva mismo envelope de control-plane y cambia solo su `payload.config`.
