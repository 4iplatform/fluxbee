# AI.frontdesk.gov

Scaffold inicial del runtime `AI.frontdesk.gov`.

Estado:
- conecta al router con `fluxbee_sdk`,
- recibe mensajes y registra trazas básicas,
- deja explicitado el flujo TODO para integración identity (`ILK_REGISTER`, `ILK_ADD_CHANNEL`).
- pendiente: handler explícito de `get_node_status` (hoy puede quedar en fallback inferido por orchestrator).

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
2. colectar identidad mínima (`email`, `display_name`, `tenant_hint`/empresa),
3. resolver tenant canónico vía `TNT_CREATE` en `SY.identity`,
4. usar el `tenant_id=tnt:<uuid>` devuelto por `TNT_CREATE` para `ILK_REGISTER`,
5. en canal adicional, invocar `ILK_ADD_CHANNEL` con merge si aplica.

Semántica actual recomendada:
- `TNT_CREATE` debe usarse como `create-or-resolve`.
- Si ya existe un tenant con el mismo `domain` o `name` normalizado, `SY.identity`
  devuelve el `tenant_id` existente en vez de crear otro.
- `ILK_REGISTER` no debe recibir nombres humanos de tenant (`"4iPlatform"`), sino
  siempre `tenant_id` canónico tipo `tnt:<uuid>`.

Checklist de aceptación:
- `docs/onworking/identity_v2_tasks.md` sección `E2`.

## Status Operativo (recomendado)

Este runtime debería responder `NODE_STATUS_GET` para evitar `health_source=ORCHESTRATOR_INFERRED`
en `GET /hives/{hive}/nodes/{node}/status`.

Consulta rápida:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="AI.frontdesk.gov@$HIVE_ID"
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/status" | jq .
```
