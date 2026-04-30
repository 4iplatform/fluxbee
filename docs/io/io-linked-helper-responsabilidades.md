# Especificación técnica — Responsabilidades del nodo `IO.linkedhelper`
## Adapter, nodo y core

> Documento actualizado para reflejar el reparto vigente de responsabilidades del nodo `IO.linkedhelper`.

---

# 1. Alcance

Este documento cubre únicamente:

- responsabilidades por componente,
- invariantes del sistema,
- límites de cada capa,
- cajas negras relevantes,
- y nuevas responsabilidades derivadas de ICH/config/pending.

No cubre en detalle:

- protocolo HTTP final;
- shape definitivo de payloads;
- wire contracts completos.

---

# 2. Principio rector

La integración con Linked Helper se organiza en tres capas:

- **adapter**: observa y actúa sobre Linked Helper;
- **nodo `IO.linkedhelper`**: coordina el estado mínimo del canal y traduce hacia Fluxbee;
- **Fluxbee/core**: resuelve identidad canónica, configuración y lógica interna del sistema.

La idea central es que:

- Linked Helper quede encapsulado detrás del adapter,
- el nodo no se convierta en un backend operacional general,
- y Fluxbee/core siga siendo fuente de verdad de identidad y configuración.

---

# 3. Responsabilidades del adapter

## 3.1. Rol general

El adapter es el componente que:

- se instala en la PC donde corre Linked Helper;
- accede al mundo técnico de LH;
- monitorea carpetas/instancias disponibles;
- detecta nuevos profiles y eventos relevantes;
- se comunica con `IO.linkedhelper`;
- y ejecuta en LH las acciones que Fluxbee le devuelva.

## 3.2. Responsabilidades claras

### A. Descubrimiento automático de instancias y profiles
Debe poder:

- monitorear las carpetas/instancias de Linked Helper;
- detectar automáticamente nuevas instancias;
- detectar nuevos profiles disponibles.

### B. Comunicación con el nodo
Debe:

- iniciar siempre la comunicación con el nodo;
- enviar eventos;
- enviar beacon/heartbeat cuando no tenga eventos;
- recibir resultados y cambios de configuración en la respuesta.

### C. Ejecución de acciones sobre LH
Debe materializar en Linked Helper acciones como:

- enviar mensajes;
- actualizar mensajes pendientes;
- borrar/cancelar mensajes pendientes;
- otras acciones operativas futuras sobre LH.

### D. Política operativa de retry/alerta
Queda del lado del adapter decidir, ante un resultado de error:

- si reintenta;
- cuándo reintenta;
- cuántas veces reintenta;
- cuándo alerta.

### E. Construcción de ids canónicos externos
Debe construir y enviar al nodo los ids canónicos externos que el canal necesite para garantizar unicidad.

En particular:

- `external_profile_id` debe llegar ya resuelto por el adapter como identificador canónico del profile;
- si para lograr unicidad el adapter necesita componerlo a partir de `adapter_id`, instancia de Linked Helper u otros componentes, esa composición pertenece al adapter;
- el nodo no debería depender de conocer ni reconstruir esa composición interna.

## 3.3. Restricciones claras

### A. No es dueño de la identidad definitiva del profile
Puede descubrir y reportar un profile, pero no define su identidad final dentro de Fluxbee.

### B. No reporta mensajes ni automatización de un profile sin ILK definitivo
Cuando detecta un profile nuevo:

- lo reporta;
- pero no reporta mensajes ni inicia automatización para ese profile hasta recibir el ILK utilizable.

### C. No recibe ni usa ILKs provisorios
El adapter no debe recibir el ILK provisorio del profile.

### D. La automatización se gobierna por ICH del canal LH
Debe reaccionar a la habilitación/deshabilitación por ICH de Linked Helper, no por ILK global.

### E. Los ICHs LH auto-detectados nacen desactivados
Por default, cuando se auto-detecta un profile/canal LH, la automatización de ese ICH nace desactivada.

## 3.4. Efecto operativo de la automatización desactivada

Cuando el adapter recibe que el ICH LH de un profile tiene automatización desactivada:

- deja de buscar/reportar mensajes para IA de ese profile;
- pero puede seguir reportando estados, alertas y eventos no conversacionales.

---

# 4. Responsabilidades del nodo `IO.linkedhelper`

## 4.1. Rol general

El nodo `IO.linkedhelper`:

- recibe eventos del adapter;
- crea o resuelve identidades provisorias cuando haga falta;
- coordina el estado mínimo necesario del canal;
- traduce al modelo Fluxbee;
- observa cambios relevantes de identity;
- mantiene una cola de resultados por adapter;
- y devuelve al adapter resultados e información de configuración.

## 4.2. Responsabilidades claras

### A. Recepción y validación inicial
Recibir y validar mínimamente los eventos enviados por el adapter.

### B. Creación de ILK provisorio para profiles nuevos
Cuando detecta un profile nuevo:

- registra un ILK de tipo `agent`;
- en estado no `complete`;
- con el tenant host/default del nodo.

### C. No exponer ILKs provisorios al adapter
El nodo no debe enviar al adapter ese ILK provisorio.

### D. Mantener listado/seguimiento de ILKs provisorios
Debe mantener un listado de ILKs provisorios del canal LH y seguir su estado.

### E. Detectar promoción del ILK observando identity SHM
Debe detectar que el ILK ya quedó listo observando la memoria compartida de identity.

### F. Devolver el ILK definitivo al adapter
Cuando detecta que el ILK está listo, lo devuelve al adapter mediante beacon/poll.

### G. Cola de resultados por adapter
Debe mantener una cola o conjunto de resultados pendientes por adapter.

### H. Gating por profile
Debe impedir que un profile no listo avance a automatización.
El bloqueo es por profile; un profile no bloquea a otro.

### I. Cambios de configuración por ICH
Debe poder devolver al adapter cambios como:
- ILK listo;
- automatización LH habilitada/deshabilitada por ICH;
- otros cambios de configuración futuros.

### J. Registro de cambios pendientes por ICH
Además de monitorear ILKs pendientes, el nodo debe llevar registro de cambios pendientes por ICH que deban informarse al adapter.

### K. Colapso del último estado por ICH
Si existe ya un cambio pendiente para un ICH y luego ocurre un nuevo cambio del mismo ICH, debe prevalecer el último estado relevante en vez de enviar ambos, salvo que en el futuro se defina explícitamente otro esquema con versionado/timestamps.

## 4.3. Monitoreo legítimo del nodo

El nodo sí debería monitorear:

- estado de profiles provisorios/listos;
- estado de sus ICHs propios;
- cambios de automatización por ICH;
- resultados pendientes por adapter;
- cambios de identidad necesarios para liberar el canal.

### Importante
Este monitoreo de estados de ICH propios no es una rareza específica de Linked Helper.
Se considera una responsabilidad generalizable para nodos IO: cada nodo IO debería poder observar los estados de sus propios ICHs y reaccionar operativamente a cambios relevantes.

## 4.4. Qué no debería absorber el nodo

No debería centralizar:

- políticas sofisticadas de retry;
- alertado operacional complejo;
- reintentos prolongados configurables;
- workflows generales de recuperación;
- decisiones de negocio posteriores a un error;
- exposición administrativa de ICHs habilitados/deshabilitados o ILKs pendientes, cuando esa responsabilidad quede del lado de identity/core.

## 4.5. Persistencia durable mínima

El nodo debería persistir de forma durable, como mínimo:

- mappings installation/adapter ↔ profile ↔ ILK/ICH;
- ILKs provisorios pendientes;
- último estado observado de automatización por ICH para LH;
- cambios pendientes de entregar al adapter;
- cola o reconstrucción de resultados por adapter.

Esto es necesario no solo para no perder seguimiento, sino para poder responderle siempre al adapter correcto.

---

# 5. Responsabilidades de Fluxbee / core / servicios internos

## 5.1. Rol general

Fluxbee/core queda como responsable de:

- sostener la fuente de verdad de identidad;
- completar datos faltantes del profile;
- promover ILKs provisorios a completos/utilizables;
- administrar configuración por ICH;
- y ejecutar routing/AI/workflows del sistema.

## 5.2. Responsabilidades claras

### A. Fuente de verdad de identidad
Los ILKs y la verdad canónica viven dentro de Fluxbee/core.

### B. Completar los datos faltantes del profile
Debe completar o confirmar, según corresponda:

- tenant representado;
- mail empresarial;
- idioma;
- huso horario;
- rol/capacidades;
- prompting/configuración futura.

### C. Promoción del ILK de profile
Debe promover el ILK desde estado provisorio a completo/utilizable.

### D. Configuración por ICH
Debe permitir habilitar/deshabilitar automatización por ICH.

### E. Exposición administrativa
La exposición administrativa de:
- ICHs habilitados/deshabilitados
- ILKs pendientes
- y otros estados de identity

ya no se considera responsabilidad del nodo `IO.linkedhelper`, sino de identity/core y de los mecanismos que defina ese equipo.

---

# 6. Invariantes claros

1. Un profile puede existir dentro de Fluxbee en estado provisorio.
2. El adapter no recibe ni usa el ILK provisorio.
3. El adapter no automatiza hasta recibir confirmación de profile listo.
4. La automatización se gobierna por ICH del canal LH.
5. Los ICHs LH auto-detectados nacen con automatización desactivada.
6. Un profile no bloquea a otro.
7. El nodo mantiene la cola de resultados por adapter.
8. El nodo detecta la promoción del profile observando identity SHM.
9. El tenant host no equivale necesariamente al tenant representado.
10. Los cambios pendientes por ICH deben llegar al adapter correcto.
11. Para cambios de ICH, debe prevalecer el último estado relevante.

---

# 7. Cajas negras / pendientes reales

## 7.1. Promoción del profile
No está definido:

- qué componente exacto la dispara;
- cómo se completa el set mínimo de datos;
- qué intervención humana ocurre exactamente;
- cómo se resuelve internamente el tenant representado.

## 7.2. `get-config` / `set-config`
Queda abierto:

- si finalmente se usan y para qué config propia del nodo;
- cómo se extienden sin deformar el modelo actual;
- y cuál será su shape final.

## 7.3. SDK
No está claro si habrá que tocar el SDK para soportar estos estados/datos extra.

---

# 8. Síntesis

## Adapter
Ve y ejecuta:
- descubre;
- reporta;
- hace polling;
- ejecuta en LH;
- decide retries/alertas.

## Nodo
Coordina y traduce:
- registra provisorios;
- observa identity;
- monitorea ILKs pendientes y estados de sus ICHs propios;
- mantiene cola;
- libera/bloquea por profile;
- registra cambios pendientes por ICH;
- devuelve resultados/config al adapter correcto.

## Core
Completa y promueve:
- mantiene identity;
- completa datos;
- promueve ILKs;
- gobierna configuración por ICH;
- expone administrativamente el estado de ICHs/ILKs por mecanismos propios;
- ejecuta routing/AI/workflows.
