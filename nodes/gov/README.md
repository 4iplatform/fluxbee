# Gov Nodes Workspace

Este directorio agrupa nodos de dominio `.gov` que forman parte del sistema Fluxbee,
pero no del core `SY.*`.

Objetivo:
- mantener separación clara entre infraestructura core y lógica de dominio,
- permitir que equipos externos desarrollen/iteren nodos `.gov` sin tocar `src/bin` core,
- compartir utilidades comunes de configuración y bootstrap.

## Estructura

```
nodes/gov/
├── common/                  # utilidades compartidas para nodos .gov
└── ai-frontdesk-gov/        # runtime AI.frontdesk.gov (scaffold inicial)
```

Alcance de esta fase:
- solo `ai-frontdesk-gov`;
- no agregar otros nodos `.gov` hasta cerrar integración y E2E de frontdesk.

## Convenciones

- Nombre L2: `AI.<servicio>.gov@<hive>`
- Runtime: `ai.<servicio>.gov`
- Configuración de ruteo de temporales:
  - `government.identity_frontdesk: "AI.frontdesk.gov@motherbee"`

## Build

Desde raíz del repo:

```bash
cargo check -p gov-common
cargo check -p ai-frontdesk-gov
```

## Integración Identity (frontdesk)

El nodo `AI.frontdesk.gov` debe usar helpers del SDK:
- `identity_system_call_ok` para `ILK_REGISTER` y `ILK_ADD_CHANNEL`
- fallback a primary (`SY.identity@<motherbee>`) cuando reciba `NOT_PRIMARY`

Checklist operativo: `docs/onworking/identity_v2_tasks.md` sección `E2`.
