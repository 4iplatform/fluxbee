# IO Slack - plan para cerrar TIMEOUT en control/config-get|set

## Contexto

Se observaron timeouts en:

- `POST /hives/{hive}/nodes/{node}/control/config-get`
- `POST /hives/{hive}/nodes/{node}/control/config-set`

incluso con nodo `active`.

## Estado actual

1. Ya se corrigio compatibilidad de control-plane para aceptar mensajes con `msg_type`:
- `system`
- `admin`

Esto era necesario porque en algunos entornos SY.admin puede enviar control-plane con `admin`.

2. Ya se corrigio arranque degradado sin secrets:
- el nodo no cae por falta de tokens,
- queda vivo para recibir `CONFIG_GET/SET`.

## Hipotesis de residual timeout

Si sigue ocurriendo timeout tras desplegar el binario nuevo, las causas probables del lado IO son:

1. **Bucle outbound bloqueante**
- control-plane y outbound Slack comparten el mismo loop de consumo.
- un envio Slack lento puede demorar respuesta de `CONFIG_GET/SET`.

2. **Falta de trazabilidad de control-plane en logs**
- hoy hay logs parciales, pero falta correlacion request->response por `trace_id` para aislar si:
  - llego request,
  - se genero response,
  - fallo `sender.send(response)`.

## Plan de cambios IO (si persiste timeout)

### P1. Observabilidad minima obligatoria

Agregar logs event-driven en `io-slack`:

- `control_request_received` (`trace_id`, `msg_type`, `msg`, `node_name`)
- `control_response_sent_ok` (`trace_id`, `response_msg`)
- `control_response_send_failed` (`trace_id`, `error`)
- `control_request_ignored` (`reason`)

Objetivo: distinguir timeout por no recepcion vs no envio vs fallo de ruta.

### P2. Prioridad de control-plane

Separar manejo de control-plane del path outbound a Slack para que no dependa de operaciones HTTP externas.

Opciones:

- A) fast-path: procesar y responder control-plane antes de cualquier trabajo outbound costoso (parcialmente ya existe, reforzar),
- B) ideal: task dedicada para control-plane con cola separada.

### P3. Test de regresion

Agregar test/integration scenario en IO:

- nodo activo sin secrets,
- `CONFIG_GET` responde < 30s,
- `CONFIG_SET` responde < 30s,
- repetir con `msg_type=system` y `msg_type=admin`.

### P4. Criterio de cierre

Se considera resuelto cuando:

1. `CONFIG_GET` no timeoutea en 3 corridas consecutivas desde spawn limpio.
2. `CONFIG_SET` responde `CONFIG_RESPONSE` consistente.
3. logs muestran request/response correlacionados por `trace_id`.

## Comando de verificacion rapida (post-deploy)

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"probe"}' | jq
```

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version":1,
    "config_version":8,
    "apply_mode":"replace",
    "config":{
      "io":{"dst_node":"AI.chat@motherbee"},
      "slack":{"app_token":"xapp-test","bot_token":"xoxb-test"}
    }
  }' | jq
```
