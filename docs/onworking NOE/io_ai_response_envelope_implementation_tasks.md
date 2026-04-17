# IO/AI Response Envelope - Tareas de implementacion

Estado: propuesta de ejecucion ordenada  
Fecha: 2026-04-17  
Base de trabajo:
- `docs/onworking NOE/io-ai-response-envelope-runtime-spec.md`
- `docs/io/io-common.md`
- `docs/io/io-api-node-spec.md`
- `docs/ai-frontdesk-gov-spec.md`
- `docs/02-protocolo.md`
- `agents-sdk-python/` como referencia primaria para nuevas capacidades del runtime AI

Alcance inicial:
- runtime compartido de `AI.*`
- `io-common`
- `IO.api`
- primer caso de prueba: `IO.api -> SY.frontdesk.gov`

Fuera de alcance inicial:
- cambios en router/core
- cambios de handoff (`frontdesk_handoff` queda sin tocar por ahora)
- soporte equivalente profundo para todos los providers no OpenAI
- rollout general a todos los nodos `AI.*`

Regla de implementación para runtime AI:

- las nuevas funcionalidades del runtime AI se portan a medida que hacen falta desde `agents-sdk-python`;
- se debe priorizar el comportamiento y las abstracciones ya existentes en ese código fuente;
- cada iteración debe portar solo el subconjunto necesario y relevante para la necesidad actual.

---

## Criterio de orden

La secuencia propuesta busca:

1. cerrar primero el contrato técnico del envelope;
2. introducir después la capacidad base en el runtime AI;
3. recién entonces agregar helpers y consumo desde `io-common` / `IO.api`;
4. usar `SY.frontdesk.gov` solo como primer caso validado, no como regla general;
5. cerrar al final tests, documentación y rollout controlado.

---

## Fase 0 - Congelar decisiones v1

- [x] RER-R0.1 Confirmar como decisión cerrada que `response_envelope` vive en `meta.context.response_envelope`.
- [x] RER-R0.1.b Confirmar como decisión cerrada que el contrato final vive en `meta.context.final_response_contract`.
- [x] RER-R0.2 Confirmar que cada nodo consumidor puede definir su propio envelope esperado.
- [x] RER-R0.3 Confirmar que `IO.api -> SY.frontdesk.gov` es solo el primer caso de adopción.
- [x] RER-R0.4 Confirmar que cuando no hay envelope, `SY.frontdesk.gov` responde texto libre.
- [x] RER-R0.5 Confirmar que `frontdesk_handoff` queda fuera del alcance de este primer corte.
- [x] RER-R0.6 Confirmar que en v1 el fallback aceptado es instrucción dinámica adicional, no prompt injection dentro del texto del usuario.

---

## Fase 1 - Shape formal del envelope

- [x] RER-R1.1 Cerrar el shape exacto de `response_envelope` v1.
- [x] RER-R1.1.b Cerrar el shape exacto de `final_response_contract` v1.
- [x] RER-R1.2 Definir reglas mínimas de validación:
  - `kind`
  - `required`
  - `properties`
  - `strict` opcional
- [x] RER-R1.3 Definir el subconjunto permitido de tipos por propiedad en v1.
- [x] RER-R1.4 Definir si v1 acepta solo top-level flat object o también objetos nested simples.
- [x] RER-R1.5 Definir política para campos opcionales no listados en `required`.
- [x] RER-R1.6 Definir qué error devuelve el runtime/consumer cuando el envelope recibido es inválido.
- [x] RER-R1.7 Documentar explícitamente que el envelope es metadata de integración y no contrato de dominio del nodo target.
- [x] RER-R1.8 Documentar explícitamente la diferencia entre contrato hop-by-hop y contrato final.

---

## Fase 2 - Modelo interno del runtime AI

- [x] RER-R2.0 Relevar en `agents-sdk-python` la capacidad equivalente exacta que se quiere portar en esta fase antes de diseñar la API Rust.
- [x] RER-R2.1 Revisar y ajustar el modelo interno del request al LLM para desacoplar:
  - instrucciones base del nodo
  - instrucciones dinámicas por turno
  - input textual
  - input parts / multimodal
  - output constraint
- [x] RER-R2.2 Definir una estructura interna equivalente a `ModelInputData` / request preparado por turno.
- [x] RER-R2.3 Identificar el punto exacto del runtime donde hoy se construye el request al modelo.
- [x] RER-R2.4 Introducir un hook interno equivalente a `call_model_input_filter`.
- [x] RER-R2.5 Garantizar que ese hook pueda leer metadata del mensaje Fluxbee antes del model call.
- [x] RER-R2.6 Garantizar que la capacidad nueva no rompa el camino actual sin envelope.

---

## Fase 3 - Structured outputs en el port Rust

- [x] RER-R3.0 Relevar en `agents-sdk-python` la implementación de structured outputs / output schema relevante para este caso antes de diseñar el port Rust.
- [x] RER-R3.1 Extender el request del LLM para soportar output schema / response format.
- [x] RER-R3.2 Agregar una abstracción de schema estructurado reutilizable dentro de `fluxbee_ai_sdk`.
- [x] RER-R3.3 Implementar la traducción `response_envelope -> output schema` para `kind = json_object_v1`.
- [x] RER-R3.3.b Definir si `final_response_contract` reutiliza exactamente el mismo shape/validator que `response_envelope`.
- [x] RER-R3.4 Integrar esa traducción con `OpenAiResponsesClient`.
- [x] RER-R3.5 Mantener un fallback limpio cuando el provider/path no soporte schema estructurado.
- [x] RER-R3.6 Validar la respuesta estructurada antes de devolverla al nodo consumidor.
- [x] RER-R3.7 Definir el error canónico cuando el modelo devuelve JSON inválido o incompleto.

---

## Fase 4 - Lectura de envelope desde mensajes Fluxbee

- [x] RER-R4.1 Implementar extracción segura de `meta.context.response_envelope` desde `Message`.
- [x] RER-R4.1.b Implementar extracción segura de `meta.context.final_response_contract` desde `Message`.
- [x] RER-R4.2 Definir helper compartido para resolver envelope desde `meta.context`.
- [x] RER-R4.3 Asegurar que replies del AI SDK no destruyan metadata necesaria para el consumer si debe conservarse.
- [ ] RER-R4.4 Definir explícitamente qué parte del `meta.context.io` vuelve en la respuesta y qué parte no.
- [ ] RER-R4.5 Evitar que `response_envelope` se mezcle con `reply_target` u otra metadata operacional no relacionada.
- [x] RER-R4.6 Definir y aplicar regla de no propagación automática de `response_envelope`.
- [x] RER-R4.7 Definir cuándo `final_response_contract` se preserva localmente y cuándo puede reenviarse como contexto pasivo.

---

## Fase 5 - Fallback textual controlado

- [x] RER-R5.1 Definir el formato exacto de la instrucción dinámica que se inyecta cuando no hay structured outputs reales.
- [x] RER-R5.2 Garantizar que esa instrucción se agregue como instrucción de sistema/runtime, no dentro del user text.
- [x] RER-R5.3 Mantener esa instrucción breve, estable y derivada del envelope.
- [x] RER-R5.4 Definir criterios para decidir cuándo usar schema real vs fallback textual.
- [x] RER-R5.5 Agregar trazabilidad/logs para saber qué camino usó cada request.

---

## Fase 6 - Helpers en `io-common`

- [x] RER-R6.1 Agregar helper para setear `meta.context.response_envelope`.
- [x] RER-R6.1.b Agregar helper para setear `meta.context.final_response_contract`.
- [x] RER-R6.2 Agregar helper para leer/parsing del envelope desde mensajes recibidos.
- [x] RER-R6.3 Agregar validación mínima del shape del envelope del lado IO.
- [x] RER-R6.4 Agregar helper para parsear respuesta JSON estructurada esperada por el consumer.
- [x] RER-R6.5 Definir errores canónicos de `io-common` para:
  - envelope inválido
  - respuesta inválida
  - respuesta incompatible con el envelope esperado

---

## Fase 7 - Integración en `IO.api`

- [x] RER-R7.1 Definir en qué path exacto `IO.api` decide que necesita envelope.
- [x] RER-R7.2 Construir el envelope para el caso síncrono de regularización de identidad.
- [x] RER-R7.3 Adjuntar ese envelope al mensaje hacia el nodo target sin mezclarlo con payload de negocio.
- [ ] RER-R7.3.b Preservar `final_response_contract` cuando `IO.api` espere una respuesta final recompuesta tras hops intermedios.
- [x] RER-R7.4 Esperar la respuesta y parsearla con helpers de `io-common`.
- [x] RER-R7.5 Mapear la respuesta estructurada al contrato HTTP de `IO.api`.
- [x] RER-R7.6 Mantener el caso v1 con status HTTP acotados, sin expandir todavía la taxonomía fina.
- [x] RER-R7.7 Asegurar que `IO.api` no asuma soporte universal del envelope en cualquier nodo target.

---

## Fase 8 - Primer caso validado con `SY.frontdesk.gov`

- [x] RER-R8.1 Definir el envelope concreto que `IO.api` le pedirá a frontdesk en el caso síncrono.
- [x] RER-R8.2 Ajustar `SY.frontdesk.gov` para que, cuando reciba envelope, produzca respuesta compatible.
- [x] RER-R8.3 Mantener texto libre cuando no haya envelope.
- [x] RER-R8.4 Asegurar que este cambio no toque todavía `frontdesk_handoff`.
- [x] RER-R8.5 Definir qué comportamiento tiene frontdesk si recibe envelope inválido.
- [x] RER-R8.6 Definir qué comportamiento tiene frontdesk si no puede cumplir el envelope pedido.
- [ ] RER-R8.7 Definir cómo preserva `final_response_contract` un nodo orquestador que hace delegación antes de responder.

---

## Fase 9 - Tests

- [x] RER-R9.1 Agregar tests unitarios del parser/validator de envelope.
- [ ] RER-R9.2 Agregar tests del hook dinámico de preparación de model input.
- [x] RER-R9.3 Agregar tests de traducción envelope -> output schema.
- [x] RER-R9.4 Agregar tests del fallback textual sin structured outputs.
- [x] RER-R9.5 Agregar tests de integración del `OpenAiResponsesClient` con response format estructurado.
- [x] RER-R9.6 Agregar tests de `io-common` para set/get/parse de envelope.
- [x] RER-R9.7 Agregar tests de `IO.api` para el caso sync con envelope.
- [x] RER-R9.8 Agregar tests de `SY.frontdesk.gov` con:
  - envelope presente
  - envelope ausente
  - envelope inválido
- [x] RER-R9.9 Agregar una validación E2E mínima `IO.api -> frontdesk -> IO.api`.

---

## Fase 10 - Documentación y rollout

- [x] RER-R10.1 Actualizar la spec de runtime AI para reflejar el nuevo modelo interno.
- [x] RER-R10.2 Actualizar `io-common` con la nueva metadata `response_envelope`.
- [x] RER-R10.3 Actualizar `IO.api` con el nuevo caso sync basado en envelope.
- [x] RER-R10.4 Revisar y actualizar la spec formal de `SY.frontdesk.gov` si el cambio de contrato queda cerrado.
- [x] RER-R10.5 Documentar explícitamente que la adopción es opt-in por nodo/runtime.
- [x] RER-R10.6 Dejar checklist operativo de rollout para nodos migrados.

---

## Validación E2E realizada

Fecha: 2026-04-17

Caso validado:

- `IO.api.support@motherbee -> SY.frontdesk.gov@motherbee -> IO.api.support@motherbee`
- request HTTP con `subject.external_user_id` nuevo
- segundo request HTTP con el mismo `external_user_id`

Resultado observado:

- primer request:
  - `IO.api` registra `identity lookup miss`
  - `IO.api` envía request síncrono a `SY.frontdesk.gov`
  - `SY.frontdesk.gov` ejecuta `ILK_REGISTER` contra `SY.identity`
  - `IO.api` recibe respuesta estructurada válida
  - el request HTTP continúa a su `dst_node` final y responde `202 Accepted`
- segundo request:
  - `IO.api` registra `identity lookup hit`
  - `registration_status=complete`
  - no vuelve a llamar a `SY.frontdesk.gov`
  - el request HTTP continúa directo al `dst_node` final y responde `202 Accepted`

Conclusión:

- el primer corte quedó validado para el caso real de regularización síncrona de identidad;
- el envelope se aplicó correctamente en el hop `IO.api -> SY.frontdesk.gov`;
- la respuesta estructurada volvió válida a `IO.api`;
- el bypass de frontdesk en requests posteriores con identidad ya regularizada funcionó como se esperaba.

---

## Orden sugerido de ejecución

1. Fase 0 - Congelar decisiones v1
2. Fase 1 - Shape formal del envelope
3. Fase 2 - Modelo interno del runtime AI
4. Fase 3 - Structured outputs en el port Rust
5. Fase 4 - Lectura de envelope desde mensajes Fluxbee
6. Fase 5 - Fallback textual controlado
7. Fase 6 - Helpers en `io-common`
8. Fase 7 - Integración en `IO.api`
9. Fase 8 - Primer caso validado con `SY.frontdesk.gov`
10. Fase 9 - Tests
11. Fase 10 - Documentación y rollout

---

## Criterio de aceptación del primer corte

Se puede considerar implementado el primer corte si:

1. el runtime AI puede leer `meta.context.response_envelope`;
2. puede convertirlo en instrucción dinámica por turno;
3. puede usar structured outputs reales en OpenAI Responses cuando aplique;
4. tiene fallback textual limpio cuando no haya schema estructurado real;
5. `io-common` expone helpers de set/get/parse del envelope;
6. `IO.api` puede pedir respuesta estructurada a un nodo target;
7. `SY.frontdesk.gov` cumple ese envelope cuando lo recibe;
8. `SY.frontdesk.gov` sigue respondiendo texto libre cuando no lo recibe;
9. `frontdesk_handoff` no fue afectado en este corte;
10. el flujo queda cubierto por tests unitarios e integración mínima;
11. `response_envelope` no se propaga automáticamente;
12. `final_response_contract` puede preservarse sin imponer shape al hop siguiente.
