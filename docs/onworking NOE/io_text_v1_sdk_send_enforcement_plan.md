# Migracion de offload text/v1 a SDK - Estado final de criterio

Fecha: 2026-04-06
Estado: criterio funcional cerrado para `text/v1`

## Objetivo

Dejar explicito donde vive hoy la normalizacion/offload de texto grande para el contrato `text/v1`.

## Criterio correcto segun codigo actual

El comportamiento automatico existe hoy para:

- mensajes cuyo `payload.type == "text"`
- y cuyo payload valida como contrato `text/v1`

Implementacion real:

1. `NodeSender::send` llama a `normalize_outbound_message(...)`
2. si el payload es `type="text"`, intenta parsearlo como `TextV1Payload`
3. si el parseo valida:
   - estima tamano
   - usa `BlobToolkit::build_text_v1_payload_with_limit(...)`
   - deja `content` inline o promueve a `content_ref`
4. si el parseo no valida:
   - loguea warning
   - forwardea el payload tal como vino

## Lo que SI significa

- para el contrato canonico `text/v1`, el offload `>64KB` ya esta centralizado en SDK
- `IO.slack` e `IO.sim` quedan cubiertos cuando envian por `sender.send(msg)`
- los productores no tienen que decidir manualmente `content` vs `content_ref` si ya trabajan dentro del contrato `text/v1`

## Lo que NO significa

- no es una interceptacion universal para cualquier JSON arbitrario
- no convierte payloads no `text/v1`
- no reemplaza la responsabilidad de cada nodo de usar el contrato canonico y el send-path correcto

## Relacion con `BlobToolkit`

La logica efectiva de corte usa el helper canonico del blob toolkit:

- `BlobToolkit::build_text_v1_payload()`
- `BlobToolkit::build_text_v1_payload_with_limit()`

Por eso, ambas afirmaciones son compatibles:

1. el corte real lo resuelve el helper de blob toolkit
2. el punto de enforcement operativo actual para envios `text/v1` al router es `NodeSender::send`

## Estado de cierre

### Cerrado

- ownership del offload para `text/v1`
- logs de normalizacion en SDK
- tests unitarios base del SDK
- smoke Linux `>64KB` con `io-sim`
- adapters IO actuales revisados sin bypass conocido

### Pendiente

- mas tests de integracion `io-common` / `IO.slack`
- dejar como requisito de adopcion para futuros adapters IO:
  - usar contrato `text/v1`
  - enviar por `NodeSender::send`

## Escape hatch

No se implementa `disable_auto_blob` por ahora.

Motivo:

- en documentos core/protocolo del repo esta normado que, si el texto no cabe inline, debe ir por `content_ref`
- no aparece en core una excepcion normativa ni un flag para desactivar ese comportamiento
- por lo tanto, el auto-offload se trata como contrato, no como opcion de rollback
