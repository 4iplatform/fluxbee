# WF Generic

Runtime base para nodos `WF.*` instanciables.

Este módulo aloja el runtime genérico del motor de workflows en Go.
El primer corte implementado fija:

- módulo y entrypoint
- tipos de definición del workflow
- carga estricta desde JSON
- validación de load-time
- compilación de guards CEL

El resto del runtime (`store`, `dispatch`, `recover`, `actions`, etc.) se
completa sobre esta base.
