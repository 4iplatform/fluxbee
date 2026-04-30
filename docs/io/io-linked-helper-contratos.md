# Especificación técnica — Contratos v1 `adapter ↔ IO.linkedhelper`
## Mensajes y respuestas mínimas

> Documento actualizado para reflejar el reparto vigente de responsabilidades del nodo `IO.linkedhelper`.

---

# 1. Alcance

Este documento cubre:

- tipos de eventos mínimos del adapter;
- tipos de respuesta mínimos del nodo;
- shape conceptual mínimo de cada familia;
- campos mínimos necesarios para avanzar.

No cubre todavía:

- schema JSON cerrado final;
- payloads avanzados;
- adjuntos en detalle;
- edición/cancelación de mensajes;
- contrato final de `get-config` / `set-config`.

---

# 2. Regla general de correlación

Todos los items enviados por el adapter deben tener un identificador correlacionable.

Toda respuesta del nodo, salvo heartbeat puro, debe poder referenciar el evento original.

## Definición mínima ya aceptable para avanzar
La correlación debe existir sí o sí.

Para la primera implementación, el criterio operativo aceptable es:

- el adapter garantiza unicidad de `event_id` dentro de sí mismo;
- el nodo trata la clave efectiva del evento como `adapter_id + event_id`;
- y las respuestas del nodo pueden referenciar el evento original devolviendo `event_id`, quedando `adapter_id` como contexto natural del canal.

## Abierto de verdad
Queda por cerrar solo el detalle fino de si se usará:
- solo `event_id`;
- `request_id + event_id`;
- u otra combinación.

---

# 3. Eventos mínimos del adapter (v1)

## 3.1. `heartbeat`

### Objetivo
Mantener el intercambio y retirar pendientes cuando el adapter no tiene nuevos eventos salientes.

### Campos mínimos conceptuales
- `id`
- `adapter_id`
- `timestamp`

### Observación
No es un simple keepalive: también sirve para retirar resultados y cambios de configuración.

---

## 3.2. `profile_create`

### Objetivo
Reportar el descubrimiento de un profile nuevo.

### Semántica actual
No implica que el adapter pueda automatizar.
Su función es permitir que el nodo cree el ILK provisorio y ponga en marcha el flujo interno.

### Campos mínimos conceptuales
- `id`
- `adapter_id`
- `external_profile_id`
- `display_name`
- `metadata` opcional

### Restricción importante
- `external_profile_id` lo genera el adapter.
- Debe garantizar unicidad global entre todos los adapters.
- El adapter no recibe el ILK provisorio resultante.
- Aunque el adapter lo construya de forma compuesta internamente si hace falta, para el nodo debe llegar ya como `external_profile_id` canónico listo para usar.

### Metadata útil potencial
- email observado
- timezone observado si existiera y fuera confiable
- otros datos visibles del profile

---

## 3.3. `conversation_message`

### Objetivo
Representar una comunicación consolidada desde un contacto externo hacia un profile.

### Restricción principal
Solo se emite para profiles que ya tengan ILK definitivo/utilizable y cuyo ICH LH tenga automatización habilitada.

### Campos mínimos conceptuales
- `id`
- `adapter_id`
- `profile_ilk`
- `contact_name`
- `contact_external_composite_id`
- `contact_lh_person_id` (referencia de origen)
- `conversation_external_id`
- `content`

### Sobre el contacto
`contact_external_composite_id` debe leerse, a efectos prácticos del nodo, como el `external_id` canónico del contacto.

Puede ser un id compuesto construido por el adapter para garantizar unicidad global, pero:

- el nodo no debería depender de desarmarlo;
- el nodo lo trata como identificador canónico opaco listo para identity resolution.

El `contact_lh_person_id`:
- es útil para trazabilidad;
- pero no debe usarse solo como identidad canónica global.

### Sobre el contenido
Debe pensarse como contenido conversacional general.
En v1 puede ser texto, pero no conviene asumir que siempre será solo mensaje textual en futuras iteraciones.

### Comportamiento MVP ante falla de resolución de contacto
Para el MVP, si el nodo no puede resolver la identidad mínima del contacto:
- no avanza la conversación;
- devuelve error;
- y no agrega complejidad extra de esperas prolongadas del lado del nodo.

---

## 3.4. `system_alert`

### Objetivo
Reportar eventos no conversacionales del canal LH.

### Casos posibles
- instancia/estado de profile;
- cambios operativos;
- alertas del canal;
- otros eventos no IA.

### Estado actual
Se reconoce como familia válida.

## Abierto de verdad
Lo que sigue pendiente es el recorte exacto del conjunto mínimo de alertas que entrará en el MVP y el shape concreto de su payload.

---

## 3.5. Evento potencial futuro: `profile_update`

Hoy **no forma parte del set mínimo v1**.

### Motivo
Con el reparto actual:
- el adapter ya no es la fuente fuerte de verdad del profile;
- Fluxbee/core concentra la mayor parte de la identidad/configuración;
- y no hay hoy suficiente señal útil del lado adapter como para justificarlo en una primera versión.

### Estado
Se deja explícitamente como:
- evento potencial futuro,
- no implementado en la primera versión.

---

# 4. Respuestas mínimas del nodo (v1)

Se mantienen como familias top-level:

- `ack`
- `result`
- `heartbeat`

---

## 4.1. `ack`

### Objetivo
Indicar que el nodo recibió y aceptó el evento para procesamiento.

### Campos mínimos conceptuales
- `response_id`
- `adapter_id`
- `event_id`
- `type = ack`

### Observación
No implica resolución final.

---

## 4.2. `result`

### Objetivo
Representar el resultado operativo actual de un evento previo.

### Campos mínimos conceptuales
- `response_id`
- `adapter_id`
- `event_id`
- `type = result`
- `status = success | error`
- `result_type`
- `payload` opcional
- `error_code` opcional
- `error_message` opcional
- `retryable` opcional

### Regla importante
`error` no es un mensaje aparte.
Es un `result` con `status = error`.

### Catálogo base ya encaminado
Para el MVP, el conjunto base ya suficientemente definido es:
- `profile_ready`
- `conversation_processed`
- `automation_enabled`
- `automation_disabled`
- `identity_error`
- `promotion_error`
- `blocked_profile`

### Sobre resultados conversacionales
No debe asumirse que el payload conversacional sea siempre `message` textual obligatorio.
Debe poder representar contenido saliente más general.

---

## 4.3. `heartbeat`

### Objetivo
Responder cuando no hay nada más que devolver o como respuesta mínima a un poll vacío.

### Campos mínimos conceptuales
- `response_id`
- `adapter_id`
- `type = heartbeat`
- `timestamp`

---

# 5. Resultados del beacon que ya quedan encaminados

## 5.1. `profile_ready`
Cuando el nodo detecta que el ILK ya pasó a estado utilizable, se lo devuelve al adapter.

### Payload mínimo esperado
- `external_profile_id`
- `ilk_id`
- `ich_id`

## 5.2. `automation_enabled`
Cuando el ICH de LH pasa a habilitado.

### Payload mínimo esperado
- `external_profile_id`
- `ilk_id`
- `ich_id`

## 5.3. `automation_disabled`
Cuando el ICH de LH pasa a deshabilitado.

### Payload mínimo esperado
- `external_profile_id`
- `ilk_id`
- `ich_id`

## 5.4. Errores de resolución/promoción
El nodo debe poder devolver errores relacionados con:
- imposibilidad de promover el profile;
- imposibilidad de resolver identidad;
- o fallas internas del flujo de registro.

---

# 6. Qué sigue vigente de contratos anteriores

Se conserva:

- `ack`
- `result`
- `heartbeat`
- `result` con `status` + `result_type`
- `conversation_message` como familia de evento válida

Se redefine:

- `profile_create` como descubrimiento que dispara ILK provisorio;
- `conversation_message` solo para profiles con ILK definitivo y automatización LH habilitada;
- `result` debe contemplar explícitamente cambios de config/ICH y profile listo.

Se elimina del mínimo v1:

- `profile_update`

---

# 7. Pendientes abiertos reales

## 7.1. Shape exacto de cada payload
Sigue pendiente cerrar el schema final.

## 7.2. `system_alert`
Sigue pendiente cerrar:
- el subconjunto mínimo del MVP;
- y el payload exacto.

## 7.3. Correlación exacta
No está cerrado el detalle fino del modelo definitivo de ids.

## 7.4. Adjuntos y payloads no textuales
No están cerrados todavía, aunque el contrato debería no bloquearlos a futuro.

## 7.5. `get-config` / `set-config`
Quedan tentativos y fuera de cierre en este documento.

---

# 8. Síntesis

Los contratos v1 mínimos quedan así:

## Adapter → nodo
- `heartbeat`
- `profile_create`
- `conversation_message`
- `system_alert`

## Nodo → adapter
- `ack`
- `result`
- `heartbeat`

Y con el criterio de que:

- el profile descubierto primero entra en flujo provisorio;
- el adapter no consume ILKs provisorios;
- `conversation_message` solo existe una vez que el profile ya está listo y habilitado para automatización por ICH;
- `profile_update` queda fuera del MVP y se deja como evento potencial futuro.
