# SY.frontdesk.gov - Especificacion tecnica (v2)

Estado: vigente

## 1. Rol

`SY.frontdesk.gov` es el nodo especializado de identity/onboarding del sistema.

Su funcion es:

- recibir casos con identidad `temporary` o incompleta;
- completar registro humano cuando el caso ya trae datos suficientes;
- continuar el flujo conversacional cuando todavia faltan datos;
- ejecutar el upgrade via `ILK_REGISTER`;
- responder siempre con un payload canonico de aplicacion.

No reemplaza a un `AI.*` generalista.

## 2. Identidad y naming

- Nombre L2 fijo: `SY.frontdesk.gov`
- Nombre calificado: `SY.frontdesk.gov@<hive_id>`

Regla operativa:

- existe una sola instancia canonica por hive;
- debe considerarse parte del `system default set`;
- puede recibir trafico derivado por nodos IO/AI u otras capas de negocio.

## 3. Responsabilidades

`SY.frontdesk.gov`:

- acepta dos contratos oficiales de input:
  - `payload.type = "text"`
  - `payload.type = "frontdesk_handoff"`
- reutiliza estado por `src_ilk`;
- completa o corrige los datos minimos del humano;
- llama a `ilk_register` cuando el caso ya esta listo;
- responde siempre con `payload.type = "frontdesk_result"`.

No debe:

- escribir directo en la identity DB;
- inventar tenants o ILKs;
- volver a inferir tenancy desde hints textuales cuando el handoff ya trae `tenant_id`;
- emitir `text/v1` como contrato canonico de salida.

## 4. Inputs oficiales

### 4.1 Conversacional

Carrier requerido:

- `meta.src_ilk`
- `meta.thread_id`
- payload `text`

Uso:

- cuando el caso viene de un canal humano normal;
- cuando faltan datos y frontdesk debe pedirlos.

### 4.2 Estructurado

Carrier requerido:

- `meta.src_ilk`
- `meta.thread_id`
- payload `frontdesk_handoff`

Shape canonico:

```json
{
  "type": "frontdesk_handoff",
  "schema_version": 1,
  "operation": "complete_registration",
  "subject": {
    "display_name": "Juan Perez",
    "email": "juan@example.com",
    "phone": "+5491100000001",
    "company_name": "Acme Support",
    "attributes": {
      "crm_customer_id": "crm-123"
    }
  },
  "tenant_id": "tnt:...",
  "context": {
    "source_node": "IO.api.support@motherbee",
    "external_user_id": "crm:123"
  }
}
```

Reglas:

- `operation = "complete_registration"` es la unica operacion cerrada en esta version;
- `subject.display_name` y `subject.email` son requeridos para esa operacion;
- `tenant_id` es opcional;
- si falta `tenant_id`, frontdesk conserva el tenant ya asociado al caso/ILK;
- frontdesk no toma tenancy desde `tenant_hint` ni desde metadata blanda.

## 5. Estado por hilo

Mantiene estado por `src_ilk` usando `thread_state_*`.

Store minimo:

- `thread_state_get`
- `thread_state_put`
- `thread_state_delete`

La clave efectiva de estado queda asociada al `src_ilk`.

Estado minimo actual:

```json
{
  "status": "collecting|awaiting_confirmation|completed|completed_error",
  "collected": {
    "name": null,
    "email": null,
    "phone": null,
    "company_name": null
  },
  "tenant_id": null,
  "registration_status": null,
  "register_attempted": false,
  "register_error": null
}
```

## 6. Modos internos de trabajo

### 6.1 `register_automatic`

Se activa cuando entra `frontdesk_handoff`.

Regla:

- no abre conversacion innecesaria;
- mergea con estado previo si corresponde;
- si ya tiene el minimo completo, intenta registrar;
- si sigue incompleto, responde `frontdesk_result` con `needs_input`.

### 6.2 Conversacional

Se activa con `payload.type = "text"`.

Regla:

- puede recolectar datos faltantes;
- puede pedir confirmacion;
- ante confirmacion positiva llama `ilk_register`;
- debe responder con `frontdesk_result`, no con `text/v1`.

## 7. Tool de completacion

El nodo usa la tool `ilk_register`.

Payload minimo:

- `src_ilk`
- `identity_candidate.name`
- `identity_candidate.email`

Opcionales:

- `identity_candidate.phone`
- `tenant_id`
- `thread_id`

Regla:

- en handoff estructurado, si `tenant_id` viene, se usa ese tenant para `ILK_REGISTER`;
- en conversacional, la resolucion de tenant sigue la logica vigente del nodo.

## 8. Output canonico

Toda salida de `SY.frontdesk.gov` debe usar:

- `meta.type = "user"`
- `payload.type = "frontdesk_result"`

Shape canonico:

```json
{
  "type": "frontdesk_result",
  "schema_version": 1,
  "status": "needs_input",
  "result_code": "MISSING_REQUIRED_FIELDS",
  "human_message": "Necesito tu email para continuar.",
  "missing_fields": ["email"],
  "error_code": null,
  "error_detail": null,
  "ilk_id": "ilk:...",
  "tenant_id": "tnt:...",
  "registration_status": "temporary"
}
```

Campos obligatorios:

- `type`
- `schema_version`
- `status`
- `result_code`
- `human_message`
- `missing_fields`
- `error_code`
- `error_detail`

Campos obligatorios cuando se conocen:

- `ilk_id`
- `tenant_id`
- `registration_status`

## 9. Semantica de resultado

Estados cerrados:

- `ok`
- `needs_input`
- `error`

`result_code` cerrados iniciales:

- `REGISTERED`
- `ALREADY_COMPLETE`
- `MISSING_REQUIRED_FIELDS`
- `INVALID_REQUEST`
- `REGISTER_FAILED`
- `IDENTITY_UNAVAILABLE`

`human_message` es obligatorio siempre.

`missing_fields`:

- si `status = "needs_input"`, contiene la lista exacta de faltantes;
- en cualquier otro caso, debe ser `[]`.

## 10. Integracion con consumidores

### 10.1 Consumidores conversacionales

Deben:

- entender `frontdesk_result`;
- extraer `human_message`;
- no asumir salida `text/v1`.

### 10.2 Consumidores no conversacionales

Deben:

- consumir el bloque estructurado completo;
- usar `status`, `result_code`, `missing_fields`, `error_code`, `error_detail`, `ilk_id`, `tenant_id`, `registration_status`.

### 10.3 `IO.api`

`IO.api` debe:

- construir `frontdesk_handoff`;
- enviar por `dst` explicito a `SY.frontdesk.gov`;
- consumir `frontdesk_result` y mapearlo a su respuesta HTTP.

## 11. Configuracion y operacion

Estado vigente:

- usa el runner compartido con `CONFIG_GET` / `CONFIG_SET`;
- si `behavior.instructions` falta, usa prompt base embebido;
- si corre como servicio del sistema sin YAML de nodo, bootstrappea `node_name` desde `hive.yaml` como `SY.frontdesk.gov@<hive_id>`;
- la key del provider se materializa localmente en `secrets.json`.

## 12. Observabilidad minima

Debe exponer:

- `state`: `UNCONFIGURED|CONFIGURED|FAILED_CONFIG`
- contadores sugeridos:
  - `threads_active`
  - `identity_upgrades_ok`
  - `identity_upgrades_error`
  - `frontdesk_handoff_ok`
  - `frontdesk_handoff_needs_input`

## 13. Dependencias abiertas de core

- `ILK_PROVISION` todavia no acepta `tenant_id` explicito;
- la version actual de `IO.api` puede validar tenant y construir handoff correcto, pero el aislamiento multitenant real del provisional sigue dependiendo del cierre de ese gap en core.
