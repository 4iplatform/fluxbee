# AI Nodes

Fuentes de nodos AI del repo.

Convención actual:
- `common/`: código compartido de la familia AI.
- `ai-generic/`: runner AI base e instanciable.

Regla:
- las instancias (`AI.chat@motherbee`, etc.) no viven en el repo,
- acá viven solo los fuentes del runtime y sus especializaciones.
