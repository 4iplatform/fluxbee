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
- catálogo exhaustivo de `result_type`;
- payloads avanzados;
- adjuntos en detalle;
- edición/cancelación de mensajes;
- contrato final de config / status administrativo.

---

# 2. Regla general de correlación

Todos los items enviados por el adapter deben tener un identificador correlacionable.

Toda respuesta del nodo, salvo heartbeat puro, debe poder referenciar el evento original.

Sigue abierto si habrá:
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

## 3.2. `avatar_create`

### Objetivo
Reportar el descubrimiento de un avatar nuevo.

### Semántica actual
No implica que el adapter pueda automatizar.
Su función es permitir que el nodo cree el ILK provisorio y ponga en marcha el flujo interno.

### Campos mínimos conceptuales
- `id`
- `adapter_id`
- `external_avatar_id`
- `display_name`
- `metadata` opcional

### Metadata útil potencial
- email observado
- timezone observado si existiera y fuera confiable
- otros datos visibles del avatar

### Restricción importante
El adapter no recibe el ILK provisorio resultante.

---

## 3.3. `conversation_message`

### Objetivo
Representar una comunicación consolidada desde un contacto externo hacia un avatar.

### Restricción principal
Solo se emite para avatares que ya tengan ILK definitivo/utilizable y cuyo ICH LH tenga automatización habilitada.

### Campos mínimos conceptuales
- `id`
- `adapter_id`
- `avatar_ilk`
- `contact_name`
- `contact_external_composite_id`
- `contact_lh_person_id` (referencia de origen)
- `conversation_external_id`
- `content`

### Sobre el contacto
El `contact_lh_person_id`:
- es útil para trazabilidad;
- pero no debe usarse solo como identidad canónica global.

### Sobre el contenido
Debe pensarse como contenido conversacional general.
En v1 puede ser texto, pero no conviene asumir que siempre será solo mensaje textual en futuras iteraciones.

---

## 3.4. `system_alert`

### Objetivo
Reportar eventos no conversacionales del canal LH.

### Casos posibles
- instancia/estado de avatar;
- cambios operativos;
- alertas del canal;
- otros eventos no IA.

### Estado actual
Se reconoce como familia válida, pero su payload exacto sigue abierto.

---

## 3.5. Evento potencial futuro: `avatar_update`

Hoy **no forma parte del set mínimo v1**.

### Motivo
Con el reparto actual:
- el adapter ya no es la fuente fuerte de verdad del avatar;
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

### Ejemplos conceptuales de `result_type`
- `avatar_ready`
- `conversation_processed`
- `automation_enabled`
- `automation_disabled`
- `identity_error`
- `promotion_error`
- `blocked_avatar`

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

# 5. Nuevos resultados relevantes con el reparto actualizado

## 5.1. Avatar listo / ILK definitivo
Este es ahora uno de los resultados más importantes del canal.
Cuando el nodo detecta que el ILK ya pasó a estado utilizable, se lo devuelve al adapter.

## 5.2. Automatización LH habilitada/deshabilitada por ICH
También debe poder devolverse como resultado/cambio del canal.

## 5.3. Errores de resolución/promoción
El nodo debe poder devolver errores relacionados con:
- imposibilidad de promover el avatar;
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

- `avatar_create` como descubrimiento que dispara ILK provisorio;
- `conversation_message` solo para avatares con ILK definitivo y automatización LH habilitada;
- `result` debe contemplar explícitamente cambios de config/ICH y avatar listo.

Se elimina del mínimo v1:

- `avatar_update`

---

# 7. Pendientes abiertos

## 7.1. Shape exacto de cada payload
Sigue pendiente cerrar el schema final.

## 7.2. `result_type` definitivo
Sigue pendiente definir el catálogo exacto.

## 7.3. `system_alert`
Sigue pendiente cerrar su payload y taxonomía.

## 7.4. Correlación exacta
No está cerrado el modelo definitivo de ids.

## 7.5. Adjuntos y payloads no textuales
No están cerrados todavía, aunque el contrato debería no bloquearlos a futuro.

## 7.6. Config / status administrativo
Queda abierto cómo exponer:
- ICHs deshabilitados;
- ILKs pendientes;
- y otros estados administrativos del nodo,
sin acoplar innecesariamente el contrato de data plane.

---

# 8. Síntesis

Los contratos v1 mínimos quedan así:

## Adapter → nodo
- `heartbeat`
- `avatar_create`
- `conversation_message`
- `system_alert`

## Nodo → adapter
- `ack`
- `result`
- `heartbeat`

Y con el criterio de que:

- el avatar descubierto primero entra en flujo provisorio;
- el adapter no consume ILKs provisorios;
- `conversation_message` solo existe una vez que el avatar ya está listo y habilitado para automatización por ICH;
- `avatar_update` queda fuera del MVP y se deja como evento potencial futuro.
