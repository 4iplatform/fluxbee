# AI.frontdesk.gov

Scaffold inicial del runtime `AI.frontdesk.gov`.

Estado:
- conecta al router con `fluxbee_sdk`,
- recibe mensajes y registra trazas básicas,
- deja explicitado el flujo TODO para integración identity (`ILK_REGISTER`, `ILK_ADD_CHANNEL`).

## Variables útiles

- `GOV_NODE_NAME` (default: `AI.frontdesk.gov`)
- `GOV_NODE_VERSION` (default: `0.1.0`)
- `GOV_ROUTER_SOCKET_DIR` (default: `/var/run/fluxbee/routers`)
- `GOV_UUID_PERSISTENCE_DIR` (default: `/var/lib/fluxbee/state/nodes`)
- `GOV_CONFIG_DIR` (default: `/etc/fluxbee`)
- `GOV_IDENTITY_TARGET` (default: `SY.identity`)
- `JSR_LOG_LEVEL` (default: `info`)

## Ejecutar local

Desde raíz del repo:

```bash
cargo run -p ai-frontdesk-gov
```

## Integración esperada

Para cerrar el flujo productivo:
1. consumir `meta.src_ilk` temporal,
2. colectar identidad mínima (`email`, `display_name`, `tenant_id`),
3. invocar `ILK_REGISTER` vía SDK (`identity_system_call_ok`),
4. en canal adicional, invocar `ILK_ADD_CHANNEL` con merge si aplica.

Checklist de aceptación:
- `docs/onworking/identity_v2_tasks.md` sección `E2`.
