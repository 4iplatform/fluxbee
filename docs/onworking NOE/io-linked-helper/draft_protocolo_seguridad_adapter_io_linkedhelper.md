# Draft — Protocolo y seguridad de comunicación Adapter ↔ IO.LinkedHelper

## Alcance de este documento

Este borrador cubre únicamente:

- el canal de comunicación entre el **adapter** y el nodo **IO.LinkedHelper**,
- decisiones de **protocolo/transporte**,
- decisiones mínimas de **seguridad**,
- y dudas abiertas relacionadas con esos dos puntos.

Quedan **fuera de alcance por ahora**:

- el mecanismo detallado de mensajes de negocio,
- los tipos concretos de mensajes,
- el formato definitivo del payload,
- el flujo detallado de updates/deletes,
- cognition,
- prompting,
- y la definición final de avatar como tipo de ILK.

---

## Definiciones ya acordadas

### 1. Canal dedicado para Linked Helper

Se define avanzar con un nodo **IO.LinkedHelper** específico para este dominio.

Este nodo será el responsable de:

- recibir comunicaciones del adapter técnico,
- traducirlas al modelo/protocolo interno de Fluxbee,
- y enviar hacia el adapter los comandos o acciones que correspondan.

No se toma el camino de reutilizar directamente un nodo `IO.api` genérico.

---

### 2. Responsabilidad del adapter técnico

El adapter técnico es el componente externo que conoce el mundo Linked Helper y es responsable de:

- acceder al software / datos de Linked Helper,
- leer información desde LH,
- materializar cambios hacia LH,
- mantener el mapping entre **avatar externo** e **ILK**,
- y comunicarse con `IO.LinkedHelper`.

El adapter es, por lo tanto, la capa dueña de la lógica de acceso técnico a Linked Helper.

Fluxbee **no** interactúa directamente con:

- la PC donde corre LH,
- la estructura interna de tablas de LH,
- ni los mecanismos técnicos de inserción/actualización/borrado en LH.

---

### 3. Altas y actualizaciones de avatar

El adapter será el **dueño del dato del avatar** y notificará al nodo `IO.LinkedHelper`:

- nuevos avatares,
- y actualizaciones de avatares existentes.

`IO.LinkedHelper`, a su vez, deberá disparar las acciones necesarias hacia los componentes de Fluxbee que correspondan (por ejemplo frontdesk / identity).

También se define que el adapter mantendrá un mapping:

- `avatar externo ↔ ilk`

para usar el ILK registrado en sus comunicaciones posteriores.

---

### 4. Necesidad de una operación de update de ILK

Se detectó que en la documentación actual de Fluxbee existen operaciones como:

- `ILK_CREATE`
- `ILK_LINK`
- `ILK_DELETE`
- `ILK_GRADUATE`

pero **no aparece una operación explícita `ILK_UPDATE`**.

Por lo tanto, queda asentado que debe elevarse al equipo core la necesidad de evaluar/agregar una operación de actualización de ILK si se quiere soportar correctamente el caso de actualización de avatares desde el adapter.

---

### 5. Tipos generales de mensajes del canal adapter ↔ IO.LinkedHelper

A nivel conceptual, el canal deberá soportar al menos dos grandes familias de mensajes:

#### a. Mensajes conversacionales / LLM
Mensajes relacionados con conversaciones entre:

- avatar
- y contacto externo

Ejemplo:

- ingreso de mensaje nuevo,
- contexto conversacional relevante,
- eventos que deban traducirse a flujo conversacional dentro de Fluxbee.

#### b. Mensajes de estado / alertas / acciones
Mensajes no conversacionales, vinculados a:

- estado de la instancia de Linked Helper,
- fallos,
- alertas,
- cambios de estado de campañas,
- errores operativos,
- eventos que deban rutearse dentro de Fluxbee para visibilidad o acción humana.

La definición detallada de estos tipos de mensaje queda pendiente.

---

### 6. Updates / deletes de mensajes pasan por Fluxbee

Se define que las operaciones de actualización o borrado de mensajes:

- **no** se harán por comunicación directa operador humano ↔ adapter,
- sino que entrarán por Fluxbee,
- y luego Fluxbee enviará al adapter la instrucción correspondiente.

El mecanismo exacto todavía no está definido.

Esto queda como definición de frontera, no como diseño final de implementación.

---

## Decisiones tomadas sobre protocolo de transporte

### 7. Canal de comunicación: HTTP + HTTPS

Se decide avanzar con:

- **HTTP** como protocolo de aplicación
- sobre **HTTPS** como protección del canal

para la comunicación:

- `adapter ↔ IO.LinkedHelper`

No se avanzará inicialmente por:

- TCP crudo con protocolo propio,
- ni por gRPC,
- ni por protobuf en esta etapa.

La elección de HTTP + HTTPS se considera suficiente para una primera versión con seguridad mínima razonable y menor complejidad operativa.

---

### 8. El canal adapter ↔ IO.LinkedHelper es externo al protocolo interno de Fluxbee

Aunque `IO.LinkedHelper` traduzca luego al protocolo interno de Fluxbee, el enlace:

- `adapter ↔ IO.LinkedHelper`

se considera una **capa externa/privada de integración**, con decisiones propias de protocolo y seguridad.

Por lo tanto:

- no está obligado a replicar el transporte interno de Fluxbee,
- ni a usar sus mismos mecanismos de framing,
- ni a conocer mecanismos internos como `blob_ref`.

La responsabilidad de traducir lo que venga del adapter al formato Fluxbee es del nodo `IO.LinkedHelper`.

---

## Decisiones tomadas sobre seguridad mínima

### 9. Seguridad mínima del canal

Se define como baseline de seguridad para esta etapa:

- **HTTPS** para cifrado del canal en tránsito,
- y **clave de instalación única** para identificar/autenticar cada adapter.

Esto se considera una base mínima razonable para la comunicación:

- `adapter ↔ IO.LinkedHelper`

sin buscar todavía una solución de seguridad completa o definitiva.

---

### 10. Clave de instalación única por adapter

Se define avanzar con un esquema de:

- **installation key única por adapter/instalación**

En términos conceptuales:

- cada instalación del adapter tendrá su propia clave,
- esa clave será validada por Fluxbee / `IO.LinkedHelper`,
- y servirá como mecanismo mínimo de autenticación del adapter.

No se avanzará con una clave compartida global embebida para todos los adapters.

---

### 11. Motivo de elegir installation key por instalación

Se considera mejor que una clave global porque permite, al menos conceptualmente:

- aislar instalaciones,
- revocar una instalación puntual,
- rotar credenciales en forma más controlada,
- y reducir el impacto de una eventual filtración.

La estrategia concreta de rotación o revocación aún no está definida.

---

## Dudas y definiciones pendientes

### 12. Dónde se almacena la installation key en el adapter

Todavía no está definido:

- dónde se guarda la clave en la máquina donde corre el adapter,
- qué nivel de protección tendrá en reposo,
- ni cómo evitar que quede demasiado expuesta.

Esta es una duda válida que debe contemplarse al momento de diseñar:

- instalación,
- configuración,
- almacenamiento local,
- y runtime del adapter.

---

### 13. Cómo se rota o revoca una installation key

Tampoco está definido todavía:

- si habrá rotación de credenciales,
- cómo se revocará una instalación comprometida,
- qué impacto tendrá una revocación en el funcionamiento del adapter,
- o qué mecanismo operativo administrará esas claves.

No bloquea el avance inicial, pero debe quedar explícitamente anotado.

---

### 14. Mecanismo exacto de autenticación HTTP

Aunque está decidido usar HTTPS + installation key, todavía no está definido:

- cómo se enviará esa clave en la request,
- si irá por header,
- si habrá esquema tipo bearer/token,
- si habrá firma adicional,
- ni cómo se modelará la validación del lado receptor.

Por ahora solo queda definido el principio general, no el detalle del mecanismo.

---

### 15. Tipo definitivo de payload

Todavía no se define:

- el formato exacto del body,
- el envelope de mensajes,
- la estructura de mensajes conversacionales,
- la estructura de mensajes de estado,
- ni el manejo de payloads más pesados.

Esto se trabajará más adelante en una etapa específica de definición del mecanismo de mensajes.

---

### 16. Updates / deletes de mensajes: mecanismo exacto

Está definido que pasarán por Fluxbee, pero no está definido todavía:

- si serán comandos,
- eventos especiales,
- rutas HTTP específicas,
- acciones administrativas,
- o alguna otra forma de modelado.

Queda como pendiente abierta.

---

## Puntos de dolor / consideraciones a no perder de vista

### 17. El adapter conoce un mundo técnico que Fluxbee no debe absorber

Una parte importante del valor del adapter es justamente encapsular:

- estructura interna de LH,
- accesos a máquinas / red local,
- semántica de tablas,
- y lógica técnica de integración.

El protocolo con `IO.LinkedHelper` debe preservar esa separación y evitar filtrar innecesariamente detalles internos de LH hacia Fluxbee.

---

### 18. Seguridad mínima no implica seguridad final

HTTPS + installation key única es suficiente como baseline inicial, pero no debe confundirse con una solución completa.

Quedan pendientes, entre otros:

- protección local de credenciales,
- rotación,
- revocación,
- observabilidad de seguridad,
- y endurecimiento del vínculo adapter ↔ nodo.

---

### 19. El canal deberá soportar tráfico heterogéneo

El mismo enlace adapter ↔ `IO.LinkedHelper` deberá poder transportar:

- eventos conversacionales,
- eventos de estado,
- alertas,
- y eventualmente comandos/operaciones de actualización.

Eso implica que el diseño posterior del mecanismo de mensajes deberá contemplar una taxonomía clara de tipos de mensaje.

---

## Resumen ejecutivo

### Definiciones cerradas hasta ahora

- Se avanzará con un nodo **`IO.LinkedHelper`** dedicado.
- El **adapter técnico** es el dueño del acceso a Linked Helper.
- El adapter es dueño del dato del avatar y notifica altas/actualizaciones.
- El adapter mantiene mapping `avatar ↔ ilk`.
- Se detecta la necesidad de una operación **`ILK_UPDATE`** en Fluxbee/core.
- El canal `adapter ↔ IO.LinkedHelper` será **HTTP + HTTPS**.
- La seguridad mínima será **HTTPS + installation key única por instalación**.
- Updates/deletes de mensajes pasarán por Fluxbee, no directo operador ↔ adapter.

### Pendientes principales

- almacenamiento seguro de installation key,
- rotación/revocación,
- mecanismo exacto de autenticación HTTP,
- formato exacto de mensajes,
- modelado preciso de updates/deletes,
- y definición del envelope/mecanismo de payloads.

