# Gov Nodes Workspace

Este directorio agrupa código de dominio `.gov` dentro de Fluxbee.

Objetivo:
- mantener separación clara entre infraestructura core y lógica de dominio,
- permitir que equipos externos desarrollen/iteren componentes `.gov` sin tocar `src/bin`,
- compartir utilidades comunes de configuración y bootstrap cuando haga falta.

## Estructura

```text
nodes/gov/
├── common/                  # utilidades compartidas para código gov
└── ai-frontdesk-gov/        # runtime actual que implementa sy.frontdesk.gov
```

Alcance de esta fase:
- el foco actual está en `ai-frontdesk-gov`;
- no agregar otros componentes `.gov` hasta cerrar integración y E2E de frontdesk;
- `ai-frontdesk-gov` sigue temporalmente cercano al runner de `nodes/ai/ai-generic`.

## Convenciones

- Nodo canónico actual: `SY.frontdesk.gov@<hive>`
- Runtime canónico actual: `sy.frontdesk.gov`
- Configuración de ruteo de temporales:
  - `government.identity_frontdesk: "SY.frontdesk.gov@motherbee"`

Control plane y secrets:
- frontdesk mantiene hoy el mismo contrato `CONFIG_GET` / `CONFIG_SET` del runner AI;
- la key de OpenAI se persiste localmente en `secrets.json`;
- el campo canónico actual es `config.secrets.openai.api_key`.

## Build

Desde raíz del repo:

```bash
cargo check -p gov-common
cargo check -p sy-frontdesk-gov --bin ai_node_runner
```

## Integración Identity (frontdesk)

El nodo `SY.frontdesk.gov` debe usar helpers del SDK:
- `identity_system_call_ok` para `ILK_REGISTER` y `ILK_ADD_CHANNEL`
- fallback a primary (`SY.identity@<motherbee>`) cuando reciba `NOT_PRIMARY`

Checklist operativo: `docs/onworking COA/identity_v2_tasks.md` sección `E2`.

## Contrato de Status (FR7)

Para cualquier componente `.gov`, el contrato operativo recomendado es:
- responder `get_node_status` vía SDK (handler default o custom),
- exponer `health_state` consistente (`HEALTHY|DEGRADED|ERROR|UNKNOWN`),
- mantener `extensions` solo para métricas runtime-specific.

Validación desde API admin:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="SY.frontdesk.gov@$HIVE_ID"
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/status" | jq .
```

Referencia:
- `docs/02-protocolo.md` (mensajes `NODE_STATUS_GET/_RESPONSE`)
- `docs/07-operaciones.md` (runbook de status por nodo)
