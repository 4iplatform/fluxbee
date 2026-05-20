# Especificación técnica v0 — Adapter `Linked Helper`
## Definiciones básicas para MVP

> Documento de trabajo para establecer la primera especificación técnica del adapter de Linked Helper.
>
> Estado: v0 / definiciones básicas.
>
> Objetivo: fijar el alcance, responsabilidades, supuestos y decisiones base del adapter para poder avanzar sobre diseño detallado e implementación.
> 2026-05-15: Nota importante. Esta documentación se preparó antes de los cambios del monitor de LH. Antes de avanzar con este documento (ya sea para re-definir o implementarlo) es **OBLIGATORIO** revisar si todos los conceptos aquí establecidos siguen siendo vigentes

---

# 1. Alcance

Este documento define el adapter de Linked Helper como un componente que:

- corre en la misma PC donde está instalado y funcionando Linked Helper;
- observa estructuras técnicas locales de Linked Helper;
- detecta instancias, profiles y conversaciones relevantes;
- persiste estado local en SQLite embebido;
- se comunica con el nodo `IO.linkedhelper` por HTTP/HTTPS;
- envía eventos hacia Fluxbee a través del nodo;
- recibe resultados/cambios por beacon;
- y ejecuta en Linked Helper las acciones que correspondan.

Este documento **no** cierra todavía:

- el schema exacto de SQLite;
- el contrato JSON final de cada payload;
- la estrategia fina de concurrencia en Rust;
- la taxonomía completa de alertas;
- el soporte de adjuntos;
- el soporte de edición/borrado de mensajes ya enviados;
- ni el mecanismo exacto de acceso a Linked Helper.

---

# 2. Tecnología base

## 2.1. Lenguaje
El adapter se implementará en **Rust**.

## 2.2. Persistencia local
El adapter utilizará **SQLite embebido** como base de datos local.

## 2.3. Rol de SQLite
SQLite se usará para:

- persistir estado local durable;
- sostener la identidad local del adapter y su configuración operativa;
- registrar entities observadas desde Linked Helper;
- registrar eventos salientes;
- registrar resultados/acciones entrantes;
- permitir reconstrucción del estado tras reinicio;
- y sostener idempotencia básica del adapter.

---

# 3. Principio rector

El adapter:

- **interpreta la estructura técnica** de Linked Helper;
- **transporta eventos canónicos** hacia el nodo;
- pero **no resuelve identidad canónica de negocio**, tenant final, capacidades, prompting ni decisiones funcionales del sistema.

En otras palabras:

- entiende **cómo leer y actuar** sobre Linked Helper;
- no decide **qué significa** eso a nivel de negocio dentro de Fluxbee.

---

# 4. Responsabilidades del adapter

## 4.1. Descubrimiento local
Debe poder:

- detectar instancias relevantes de Linked Helper;
- detectar profiles disponibles;
- detectar conversaciones/mensajes relevantes;
- y mantener actualizado ese estado localmente.

## 4.2. Construcción de ids externos canónicos
Debe construir los ids externos canónicos necesarios para el canal, en especial:

- `external_profile_id`
- ids canónicos de contacto, cuando aplique

Estos ids deben llegar al nodo ya listos para usar.

## 4.3. Persistencia local
Debe persistir el estado mínimo necesario para:

- no reenviar eventos ya enviados;
- no perder seguimiento al reiniciar;
- reconstruir colas locales;
- y sostener idempotencia de eventos y acciones.

## 4.4. Consolidación de mensajes
Debe consolidar localmente los mensajes conversacionales antes de emitir `conversation_message`.

La consolidación pertenece al adapter porque:

- Linked Helper ya persiste los mensajes de manera apta para consolidarlos;
- el adapter conoce la estructura real de datos de LH;
- y el nodo no debería absorber lógica específica del almacenamiento interno de LH.

## 4.5. Comunicación con el nodo
Debe:

- iniciar siempre la comunicación con el nodo;
- enviar eventos;
- hacer polling/beacon periódico;
- recibir `ack`, `result` y `heartbeat`;
- y actualizar su estado local en función de esas respuestas.

## 4.6. Ejecución de acciones en LH
Debe ejecutar en Linked Helper las acciones que el sistema le devuelva por el canal, dentro del alcance del MVP.

## 4.7. Monitoreo operativo
Debe detectar y registrar fallas operativas propias o del acceso a LH, pudiendo emitir alertas cuando corresponda.

---

# 5. Qué NO hace el adapter

El adapter no debe:

- definir la identidad definitiva del profile;
- decidir el tenant representado;
- resolver capacidades, prompting o rol del profile;
- exponer la fuente de verdad del estado de ICH;
- ser responsable de la administración global de ILKs/ICHs;
- ni convertirse en un backend de negocio paralelo.

Eso pertenece al nodo y/o a core según corresponda.

---

# 6. Relación con el nodo `IO.linkedhelper`

El adapter se relaciona con el nodo bajo estos principios:

- el nodo es el punto de traducción hacia Fluxbee;
- el adapter no consume ILKs provisorios;
- el adapter no automatiza para un profile hasta recibir su estado listo/utilizable;
- la automatización se gobierna por el ICH del canal;
- el adapter reacciona a `automation_enabled` / `automation_disabled`;
- y el nodo sigue siendo quien observa la promoción del ILK y los cambios de sus ICHs propios.

Esto es consistente con la base documental actual del nodo. fileciteturn37file0turn37file1turn37file2turn37file3

---

# 7. Modelo básico de operación

## 7.1. Observación
El adapter observa el estado local de Linked Helper.

## 7.2. Normalización
Transforma lo observado en eventos canónicos del canal.

## 7.3. Persistencia
Persiste esos eventos y su estado local en SQLite.

## 7.4. Envío
Envía los eventos al nodo en el siguiente ciclo de polling/beacon.

## 7.5. Recepción
Recibe respuestas/resultados/cambios del nodo.

## 7.6. Aplicación
Aplica localmente las acciones necesarias en LH.

## 7.7. Confirmación local
Persiste el resultado de aplicación, éxito o error.

---

# 8. Consolidación de mensajes

## 8.1. Decisión
Para el MVP, la consolidación ocurre en el adapter.

## 8.2. Motivo
La fuente real de mensajes es Linked Helper y el adapter conoce cómo se almacenan.

## 8.3. Tipo de consolidación
La consolidación será inicialmente:

- mecánica;
- determinista;
- basada en la estructura técnica observada en LH;
- no “inteligente” ni semántica de negocio.

## 8.4. Resultado esperado
El adapter emite un `conversation_message` consolidado como unidad conversacional usable por el nodo.

---

# 9. Idempotencia y deduplicación local

## 9.1. Principio
La deduplicación que importa en el adapter no es la deduplicación de datos crudos de LH, sino la de:

- eventos salientes;
- acciones/resultado entrantes.

## 9.2. Reglas mínimas
El adapter debe evitar:

- enviar dos veces el mismo `profile_create`;
- enviar dos veces el mismo bloque consolidado de `conversation_message`;
- ejecutar dos veces la misma acción lógica recibida del nodo.

## 9.3. Consecuencia
La persistencia local debe permitir saber:

- qué evento ya fue emitido;
- qué evento fue reconocido;
- qué acción ya fue aplicada;
- y qué quedó pendiente.

---

# 10. Cambios y borrados de mensajes

## 10.1. Alcance MVP
El MVP del adapter contempla **detección de nuevos mensajes**, no edición/borrado de mensajes ya enviados.

## 10.2. Motivo
No hay todavía suficiente certeza técnica sobre:

- soporte real y consistente de LinkedIn para edición/borrado post-envío;
- visibilidad confiable de esos cambios desde Linked Helper;
- ni valor suficiente para cargar esa complejidad en la primera versión.

## 10.3. Futuro
Queda como posible extensión posterior si se verifica que LH lo expone de forma consistente y útil.

---

# 11. Alertas propias del adapter

## 11.1. Principio
El adapter puede emitir alertas propias, pero de forma acotada.

## 11.2. Tipo de alertas esperables
Ejemplos:

- imposibilidad de acceder a estructuras necesarias de LH;
- inconsistencia fuerte en datos observados;
- imposibilidad de persistir en SQLite;
- imposibilidad prolongada de comunicación con el nodo;
- imposibilidad de ejecutar una acción local en LH;
- otros fallos técnicos del propio adapter.

## 11.3. Límite
El adapter no debe convertirse en un sistema de monitoreo complejo ni emitir alertas arbitrarias.

---

# 12. Profiles y automatización

## 12.1. Profiles nuevos
Cuando el adapter detecta un profile nuevo:

- lo reporta;
- pero no inicia automatización para ese profile;
- y no usa ningún ILK provisorio.

## 12.2. Activación
Solo comienza a automatizar cuando recibe por el canal que el profile quedó listo y su automatización está habilitada.

## 12.3. Desactivación
Cuando recibe `automation_disabled`:

- deja de buscar/reportar mensajes para automatización de ese profile;
- aunque puede seguir observando y reportando estados/alertas no conversacionales si corresponde.

---

# 13. Persistencia local en SQLite

## 13.1. Decisión
SQLite embebido será la persistencia local oficial del adapter.

## 13.2. Funciones mínimas que debe cubrir
Debe poder sostener:

- configuración local del adapter;
- estado de instancias detectadas;
- estado de profiles detectados;
- mapping de ids externos;
- eventos salientes;
- resultados/acciones entrantes;
- ejecución local de acciones;
- errores/reintentos básicos;
- y marcas de idempotencia.

## 13.3. Estado
El modelo exacto de tablas/entidades queda pendiente de definición detallada en la próxima iteración.

---

# 14. Polling y beacon

## 14.1. Principio
El adapter inicia siempre la comunicación con el nodo.

## 14.2. Comportamiento
En cada ciclo:

- si tiene eventos, los envía;
- si no tiene eventos, envía heartbeat;
- y en ambos casos recibe resultados/cambios pendientes del nodo.

## 14.3. Estado
La frecuencia, tamaño de lote y prioridades quedan para el diseño detallado.

---

# 15. Acciones mínimas del MVP

Para el MVP, el adapter debe estar pensado para soportar al menos:

- detección de profiles;
- envío de `profile_create`;
- consolidación y envío de `conversation_message`;
- recepción de `profile_ready`;
- recepción de `automation_enabled` / `automation_disabled`;
- ejecución básica de envío de mensajes en LH;
- persistencia local del resultado de esas acciones;
- y manejo básico de errores.

---

# 16. Seguridad local

## 16.1. Estado actual
La seguridad local queda como tema a detallar más adelante.

## 16.2. Base mínima
Debe contemplarse al menos:

- ubicación segura de installation key;
- ubicación y permisos de la SQLite;
- ubicación y permisos de logs;
- y comportamiento básico ante copia o reinstalación del adapter.

---

# 17. Observabilidad mínima

El adapter debería ofrecer, como mínimo:

- logs estructurados;
- visibilidad del estado del polling;
- visibilidad del backlog local;
- visibilidad de errores de ejecución;
- y última comunicación exitosa/fallida con el nodo.

El mecanismo concreto queda para el diseño detallado.

---

# 18. Temas que quedan para la siguiente iteración

1. modelo de datos SQLite
2. pipeline interno exacto del adapter
3. frecuencia y política de polling/lotes
4. ejecución concreta de acciones sobre LH
5. categorías de error y retries
6. concurrencia interna en Rust
7. mecanismo exacto de acceso a LH
8. taxonomía mínima de `system_alert`
9. observabilidad concreta
10. seguridad local detallada

---

# 19. Síntesis

El adapter de Linked Helper, en esta v0, queda definido como un componente en Rust con SQLite embebido que:

- observa Linked Helper localmente;
- interpreta su estructura técnica;
- consolida mensajes conversacionales;
- construye ids externos canónicos;
- persiste estado local durable;
- emite eventos al nodo `IO.linkedhelper`;
- recibe resultados/cambios por beacon;
- ejecuta acciones locales en LH;
- y mantiene idempotencia y manejo básico de errores.

Esta base ya es suficiente para avanzar a una especificación más detallada centrada en:
- modelo de datos SQLite,
- pipeline interno,
- y ejecución de acciones.
