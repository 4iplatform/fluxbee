# gov-common

Librerí­a compartida para runtimes de la familia `gov`.

Objetivo:
- concentrar helpers y contratos reutilizables de `SY.frontdesk.gov`,
- evitar que frontdesk dependa de `nodes/ai/common`,
- sostener la separación de ownership entre familia `AI` y familia `gov`.

Estado actual:
- helpers base de env/config de nodo gov,
- contrato compartido para configuración de bridge de identidad gov,
- normalización común de error payload para tools de identidad.

Dirección:
- mover gradualmente lógica reusable de `nodes/gov/ai-frontdesk-gov` a este crate,
- mantener `AI.common` y `SY.frontdesk.gov` sin dependencia cruzada.
