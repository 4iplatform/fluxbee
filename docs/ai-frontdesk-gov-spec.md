# SY.frontdesk.gov - Especificacion tecnica (v1)

Estado: vigente

## 1. Rol

`SY.frontdesk.gov` es el frontdesk de government/identity del sistema.
Su funcion es completar el upgrade de identidad cuando el interlocutor sigue en estado `temporary`.

No reemplaza a `AI.default`.
Su rol es especifico de identidad y onboarding.

## 2. Identidad y naming

- Nombre L2 fijo: `SY.frontdesk.gov`
- Nombre calificado: `SY.frontdesk.gov@<hive_id>`

Direccion vigente:

- existe una sola instancia canonica por hive
- debe considerarse parte del `system default set`
- el router puede derivar mensajes a este nodo cuando el `src_ilk` esta en estado `temporary`

## 3. Responsabilidades

`SY.frontdesk.gov`:

- recibe conversaciones con `src_ilk` temporal o identidad incompleta
- recolecta los datos minimos de identidad
- pide confirmacion explicita
- ejecuta el upgrade llamando a Identity via `ilk_register`
- limpia el estado del hilo cuando el upgrade termina

No debe:

- escribir directamente en la identity DB
- inventar tenants o ILKs
- actuar como catch-all general del sistema

## 4. Inputs esperados

El mensaje entrante debe incluir:

- `meta.src_ilk`
- `thread_id`
- payload `text/v1` con `content` y/o `attachments`

El carrier canonico para `src_ilk` es `meta.src_ilk`.

## 5. Estado por hilo

Mantiene un JSON por `src_ilk` usando `thread_state_*`.

Store minimo:

- `thread_state_get`
- `thread_state_put`
- `thread_state_delete`

La clave efectiva de estado queda asociada al `src_ilk`.

## 6. Flujo conversacional

Flujo base:

1. leer `thread_state`
2. extraer y mergear datos nuevos
3. persistir cambios
4. pedir solo faltantes
5. cuando ya estan todos los datos, resumir y pedir confirmacion
6. ante confirmacion positiva, llamar `ilk_register` en ese mismo turno
7. responder exito o error final y cortar el loop

Reglas de producto vigentes:

- no volver a pedir campos ya presentes en `thread_state` salvo correccion explicita
- no volver a pedir confirmacion si el estado ya es `awaiting_confirmation` y la respuesta es positiva

## 7. Accion de completacion

El nodo usa la tool `ilk_register`.

Payload minimo esperado:

- `src_ilk`
- `thread_id`
- `identity_candidate`
  - `name`
  - `email`
  - `phone` si aplica
  - `tenant_hint`

Resultado esperado:

- exito: el mismo `src_ilk` pasa a identidad completa
- error: devuelve razon canonica y el nodo responde error final

## 8. Data plane

El nodo responde por el mismo canal de entrada usando la metadata de IO.

Para mensajes normales:

- `text/v1`

Para errores:

- `payload.type="error"` si el consumer lo soporta
- o texto de fallback controlado

## 9. Configuracion y operacion

Estado vigente:

- `SY.frontdesk.gov` usa hoy el runner compartido que expone `CONFIG_GET` / `CONFIG_SET`
- el prompt funcional base pertenece al runtime `SY.frontdesk.gov`
- si `behavior.instructions` falta, el runtime usa un prompt base embebido
- si corre como servicio del sistema sin YAML de nodo, bootstrappea su `node_name` desde `hive.yaml` como `SY.frontdesk.gov@<hive_id>`
- la key de provider entra temporalmente por `CONFIG_SET` y luego se persiste en `secrets.json`

Esto es una solucion operativa transitoria.
La decision final sobre surface de secretos y bootstrap queda pendiente de `CORE`.

## 10. Defaults actuales

- `multimodal=false` por default, salvo override explicito
- secret canonico:
  - `config.secrets.openai.api_key`
- provider base:
  - OpenAI

## 11. Integracion con core

`SY.frontdesk.gov` depende de estas aristas de core:

- `government.identity_frontdesk` en `hive.yaml`
- routing por `registration_status=temporary`
- autorizacion en `SY.identity` para:
  - `ILK_REGISTER`
  - `ILK_ADD_CHANNEL`
  - `TNT_CREATE`

## 12. Observabilidad minima

Debe exponer:

- `state`: `UNCONFIGURED|CONFIGURED|FAILED_CONFIG`
- contadores sugeridos:
  - `threads_active`
  - `identity_upgrades_ok`
  - `identity_upgrades_error`

## 13. Pendientes de core

- definir bootstrap/secret model final para un nodo `SY` con configuracion AI
- confirmar si `CONFIG_SET` sigue siendo surface temporal o definitivo
- cerrar naming/config canonica en `hive.yaml`
