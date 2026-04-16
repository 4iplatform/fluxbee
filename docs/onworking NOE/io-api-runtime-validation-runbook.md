# IO.api - Runbook de deploy, spawn y validacion runtime

**Fecha:** 2026-04-10
**Alcance:** despliegue, spawn, configuracion y pruebas manuales de `IO.api`
**Estado:** aplicable al primer corte implementado en `nodes/io/io-api`

---

## 1. Objetivo

Este runbook deja el camino operativo minimo para:

- publicar el runtime `IO.api`;
- ejecutar `SYSTEM_UPDATE` en el hive destino;
- crear la instancia por `SPAWN_NODE`;
- aplicar configuracion valida por `CONFIG_SET`;
- probar `GET /`;
- probar `POST /` con:
  - `application/json`
  - `multipart/form-data`

Tambien deja claro cuando conviene usar `curl` y cuando Postman alcanza.

---

## 2. Estado actual del nodo

El primer corte implementado de `IO.api` soporta hoy:

- runtime `IO.api`
- `GET /`
- `POST /`
- auth `Authorization: Bearer <token>`
- `subject_mode = explicit_subject`
- `subject_mode = caller_is_subject`
- `application/json`
- `multipart/form-data`
- attachments a blob
- relay comun `config.io.relay.*`
- control plane `CONFIG_GET` / `CONFIG_SET`
- materializacion local de API keys en `secrets.json`

Todavia no cubre:

- outbound webhook
- auth HMAC o mTLS
- `mapped_subject`
- helper de deploy especifico como `deploy-io-slack.sh`

Por eso, para `IO.api` el camino de deploy hoy es el canonico generico:

1. build release
2. publish runtime
3. `SYSTEM_UPDATE`
4. `SPAWN_NODE`
5. `CONFIG_SET`

---

## 3. `curl` o Postman

Se puede probar de las dos formas.

Regla practica:

- `curl`: mejor para runbook operativo, reproducibilidad, shell remoto y evidencia facil de pegar
- Postman: util para pruebas manuales interactivas, sobre todo JSON y multipart

Recomendacion:

- usar `curl` para:
  - publish/update/spawn/config
  - pruebas basicas reproducibles
- usar Postman si queres:
  - inspeccionar respuestas comodo
  - armar multipart manualmente
  - repetir requests HTTP sin reescribir comandos

No necesitás una consola separada obligatoriamente. Si Postman llega a la URL HTTP del nodo, sirve.

---

## 4. Prerrequisitos

- hive con `SY.orchestrator`, `SY.admin` y `rt-gateway` operativos
- Admin API accesible
- repo actualizado en el host
- toolchain Rust disponible
- permisos para escribir en `dist/` si vas a publicar runtime
- un `dst_node` valido al que `IO.api` pueda enviar el inbound

Variables sugeridas:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
RUNTIME="IO.api"
VERSION="0.1.0"
NODE_NAME="IO.api.frontdesk@$HIVE_ID"
TENANT_ID="T126"
DST_NODE="resolve"
LISTEN_ADDRESS="127.0.0.1"
LISTEN_PORT="18080"
```

Notas:

- `DST_NODE="resolve"` deja el comportamiento alineado con el resto de los IO actuales.
- `LISTEN_ADDRESS` y `LISTEN_PORT` pertenecen al listener HTTP del nodo, no a la Admin API.

---

## 5. Build local del runtime

```bash
cargo build --release --manifest-path nodes/io/Cargo.toml -p io-api
```

Binario esperado:

```bash
nodes/io/target/release/io-api
```

Si queres validar antes del publish:

```bash
cargo check --manifest-path nodes/io/Cargo.toml -p io-common -p io-api
cargo test --manifest-path nodes/io/Cargo.toml -p io-common -p io-api
```

---

## 6. Publish del runtime

`IO.api` todavia no tiene helper propio estilo `publish-io-runtime.sh --kind api`, asi que hoy corresponde usar el script generico:

```bash
bash scripts/publish-runtime.sh \
  --runtime "$RUNTIME" \
  --version "$VERSION" \
  --binary "nodes/io/target/release/io-api" \
  --set-current \
  --sudo
```

Guardar de la salida:

- `manifest_version`
- `manifest_hash`

Ejemplo de variables derivadas:

```bash
MANIFEST_VERSION="<valor devuelto por publish-runtime.sh>"
MANIFEST_HASH="<valor devuelto por publish-runtime.sh>"
```

Importante:

- el `start.sh` generado por `publish-runtime.sh` no injecta `RUST_LOG` default como si hace hoy `publish-io-runtime.sh` para Slack/Sim.
- si queres logs utiles de `IO.api`, definí `RUST_LOG` en el entorno del nodo o del host antes de probar.

Nivel recomendado para esta etapa:

```bash
RUST_LOG="info,io_api=debug,io_common=debug,fluxbee_sdk=info"
```

---

## 7. `SYSTEM_UPDATE`

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
  -H "Content-Type: application/json" \
  -d "{
    \"category\":\"runtime\",
    \"manifest_version\": $MANIFEST_VERSION,
    \"manifest_hash\": \"$MANIFEST_HASH\",
    \"runtime\": \"$RUNTIME\",
    \"runtime_version\": \"$VERSION\"
  }"
```

Esperado:

- `status = ok`

Si tu entorno requiere `sync-hint` antes del update:

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
  -H "Content-Type: application/json" \
  -d '{"channel":"dist","wait_for_idle":true,"timeout_ms":30000}'
```

---

## 8. Spawn del nodo

`IO.api` hoy necesita como bootstrap minimo:

- `listen.address`
- `listen.port`

El resto de la configuracion de negocio puede entrar despues por `CONFIG_SET`.

### Spawn minimo

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\": \"$NODE_NAME\",
    \"runtime\": \"$RUNTIME\",
    \"runtime_version\": \"current\",
    \"tenant_id\": \"$TENANT_ID\",
    \"config\": {
      \"tenant_id\": \"$TENANT_ID\",
      \"listen\": {
        \"address\": \"$LISTEN_ADDRESS\",
        \"port\": $LISTEN_PORT
      }
    }
  }"
```

Lectura esperada:

- el proceso del nodo deberia arrancar;
- el nodo puede quedar `UNCONFIGURED` si todavia no tiene auth/ingress/relay completos;
- eso es correcto para esta fase.

### Si queres recrear una instancia existente

```bash
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME"
```

y despues volver a ejecutar el `POST /nodes`.

No hace falta respawn para cada prueba HTTP. Respawneá solo si cambiaste binario/runtime.

---

## 9. Confirmar listener y estado inicial

### `CONFIG_GET`

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"archi"}'
```

Antes de un `CONFIG_SET` valido, lo esperable es:

- `state = UNCONFIGURED` o equivalente redacted
- `effective_config` parcial o nulo

### Logs

Segun el unit real del nodo, revisar `journalctl`.

Ejemplo de unit derivado:

```bash
sudo journalctl -u fluxbee-node-IO.api.frontdesk-motherbee --since "10 min ago" --no-pager
```

Senales utiles al boot:

- `io-api loaded spawn config`
- `io-api http listener ready`
- `connected to router`

Si no ves logs de `io_api`, revisar `RUST_LOG`.

---

## 10. `CONFIG_SET` para una instancia `explicit_subject`

Este es el caso mas directo para la primera prueba.

### Payload recomendado

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version": 1,
    "config_version": 3,
    "apply_mode": "replace",
    "config": {
      "listen": {
        "address": "127.0.0.1",
        "port": 18081
      },
      "auth": {
        "mode": "api_key",
        "api_keys": [
          {
            "key_id": "frontdesk-main",
            "tenant_id": "tnt:frontdesk-main",
            "token": "fb_api_frontdesk_demo_001"
          }
        ]
      },
      "ingress": {
        "subject_mode": "explicit_subject",
        "accepted_content_types": [
          "application/json",
          "multipart/form-data"
        ],
        "max_request_bytes": 262144,
        "max_attachments_per_request": 4,
        "max_attachment_size_bytes": 10485760,
        "max_total_attachment_bytes": 20971520,
        "allowed_mime_types": [
          "image/png",
          "image/jpeg",
          "application/pdf",
          "text/plain",
          "application/json"
        ]
      },
      "io": {
        "dst_node": "resolve",
        "relay": {
          "window_ms": 0,
          "max_open_sessions": 10000,
          "max_fragments_per_session": 8,
          "max_bytes_per_session": 262144
        }
      }
    }
  }'
```

Esperado:

- `ok = true`
- `apply.hot_applied` con cambios de auth/io.relay/dst
- `restart_required` puede listar `listen.*` si el listener ya estaba abierto con otro valor

Notas:

- el token inline se materializa a `secrets.json`;
- `CONFIG_GET` posterior debe devolver el secreto redacted o como `local_file:<storage_key>`.
- el `storage_key` materializado reemplaza caracteres no seguros:
  - ejemplo: `frontdesk-main` termina en `io_api_key__frontdesk_main`
  - no reutilizar literalmente el `key_id` con guiones dentro de `token_ref`

---

## 11. `CONFIG_SET` para `caller_is_subject`

En este modo cada API key necesita `caller_identity`.

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version": 1,
    "config_version": 2,
    "apply_mode": "replace",
    "config": {
      "listen": {
        "address": "127.0.0.1",
        "port": 18080
      },
      "auth": {
        "mode": "api_key",
        "api_keys": [
          {
            "key_id": "portal-user-1",
            "tenant_id": "tnt:portal-user-1",
            "token": "fb_api_portal_demo_001",
            "caller_identity": {
              "external_user_id": "portal:user-123",
              "display_name": "Usuario Portal",
              "email": "usuario@example.com"
            }
          }
        ]
      },
      "ingress": {
        "subject_mode": "caller_is_subject",
        "accepted_content_types": [
          "application/json",
          "multipart/form-data"
        ],
        "max_request_bytes": 262144,
        "max_attachments_per_request": 4,
        "max_attachment_size_bytes": 10485760,
        "max_total_attachment_bytes": 20971520,
        "allowed_mime_types": [
          "image/png",
          "image/jpeg",
          "application/pdf",
          "text/plain",
          "application/json"
        ]
      },
      "io": {
        "dst_node": "resolve",
        "relay": {
          "window_ms": 0,
          "max_open_sessions": 10000,
          "max_fragments_per_session": 8,
          "max_bytes_per_session": 262144
        }
      }
    }
  }'
```

Regla importante:

- si despues probas `POST /messages` en este modo, no mandes `subject` en el payload.

---

## 12. Probar `GET /`

### `curl`

```bash
curl -sS "http://$LISTEN_ADDRESS:$LISTEN_PORT/"
```

Esperado en configured:

- `status = configured`
- `runtime = IO.api`
- `auth.mode = api_key`
- `ingress.subject_mode` segun la config aplicada
- `ingress.accepted_content_types`
- `relay.effective`
- `secrets` redacted

### Postman

- metodo: `GET`
- URL: `http://127.0.0.1:18080/`
- sin auth requerida

---

## 13. Probar `POST /` JSON con `explicit_subject`

### `curl`

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer fb_api_frontdesk_demo_001" \
  -d '{
    "subject": {
      "external_user_id": "crm:client-12345",
      "display_name": "Juan Perez",
      "email": "juan@example.com",
      "company_name": "Acme Support",
      "attributes": {
        "crm_customer_id": "crm:client-12345"
      }
    },
    "message": {
      "text": "Necesito ayuda con mi alta",
      "external_message_id": "crm-msg-001"
    },
    "options": {
      "metadata": {
        "source_system": "crm-dotnet",
        "conversation_id": "case-12345"
      }
    }
  }'
```

Esperado:

- HTTP `202`
- body con:
  - `status = accepted`
  - `request_id`
  - `trace_id` o `null`
  - `relay_status = held` o `flushed_immediately`
  - `node_name`

### Postman

- metodo: `POST`
- URL: `http://127.0.0.1:18080/`
- header `Authorization: Bearer fb_api_frontdesk_demo_001`
- body `raw`, `JSON`

---

## 14. Probar `POST /` JSON con `caller_is_subject`

### `curl`

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer fb_api_portal_demo_001" \
  -d '{
    "message": {
      "text": "Necesito ayuda con mi alta",
      "external_message_id": "portal-msg-001"
    },
    "options": {
      "metadata": {
        "conversation_id": "portal-thread-123"
      }
    }
  }'
```

Esperado:

- HTTP `202`
- sin `subject` en el request

Caso de error esperado:

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer fb_api_portal_demo_001" \
  -d '{
    "subject": {
      "external_user_id": "no-deberia-estar"
    },
    "message": {
      "text": "hola"
    }
  }'
```

Esperado:

- HTTP `422`
- `error_code = invalid_payload`

---

## 15. Probar `POST /` multipart con attachment

### `curl`

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Authorization: Bearer fb_api_frontdesk_demo_001" \
  -F 'metadata={
    "subject": {
      "external_user_id": "crm:client-12345",
      "display_name": "Juan Perez"
    },
    "message": {
      "text": "Adjunto documentacion",
      "external_message_id": "crm-msg-002"
    },
    "options": {
      "metadata": {
        "conversation_id": "case-12345"
      }
    }
  };type=application/json' \
  -F "file=@/tmp/dni-frente.png;type=image/png"
```

Notas:

- `metadata` es obligatorio;
- cualquier parte distinta de `metadata` se trata como attachment;
- el MIME tiene que estar permitido por `ingress.allowed_mime_types`.

Esperado:

- HTTP `202`
- `relay_status = held` o `flushed_immediately`

### Postman

- metodo: `POST`
- URL: `http://127.0.0.1:18080/`
- header `Authorization: Bearer fb_api_frontdesk_demo_001`
- body `form-data`
- campo `metadata`: tipo `Text`, contenido JSON valido
- campo `file`: tipo `File`, adjuntar el archivo

---

## 16. Probar relay

### Relay deshabilitado o passthrough

Config:

- `config.io.relay.window_ms = 0`

Comportamiento esperado:

- el nodo responde `202`
- normalmente `relay_status = flushed_immediately`
- en logs deberias ver `relay passthrough immediate` del relay comun

### Relay con hold/consolidacion

Aplicar una version nueva de config:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version": 1,
    "config_version": 3,
    "apply_mode": "replace",
    "config": {
      "listen": {
        "address": "127.0.0.1",
        "port": 18080
      },
      "auth": {
        "mode": "api_key",
        "api_keys": [
          {
            "key_id": "frontdesk-main",
            "token_ref": "local_file:io_api_key__frontdesk_main"
          }
        ]
      },
      "ingress": {
        "subject_mode": "explicit_subject",
        "accepted_content_types": [
          "application/json",
          "multipart/form-data"
        ],
        "max_request_bytes": 262144,
        "max_attachments_per_request": 4,
        "max_attachment_size_bytes": 10485760,
        "max_total_attachment_bytes": 20971520,
        "allowed_mime_types": [
          "image/png",
          "image/jpeg",
          "application/pdf",
          "text/plain",
          "application/json"
        ]
      },
      "io": {
        "dst_node": "resolve",
        "relay": {
          "window_ms": 2500,
          "max_open_sessions": 10000,
          "max_fragments_per_session": 8,
          "max_bytes_per_session": 262144
        }
      }
    }
  }'
```

Mandar dos requests seguidos dentro de la ventana.

Esperado:

- primera respuesta con `relay_status = held`
- es esperable que la segunda tambien responda `held` si el flush sucede despues, por scheduler, dentro de la ventana
- logs con:
  - `relay session opened`
  - `relay fragment buffered`
  - `relay session flushed`
  - `sent to router`

Lectura operativa:

- si no aparecen todos los `debug` de `io_common::relay`, pero ves:
  - `CONFIG_SET applied ... hot_applied="io.relay.*"`
  - respuestas HTTP con `relay_status = held`
  - `sent to router ... context="relay scheduled flush"`
- entonces la validacion funcional del relay puede darse igualmente por buena y el faltante es de observabilidad, no de comportamiento

---

## 17. Validacion de materializacion de attachments

Si ya probaste `multipart/form-data` y queres confirmar que el attachment quedo materializado, no uses `cd` directo al blob root si tu usuario no tiene permisos. Usá `sudo find`.

Blob root por default, si no se configuro otro:

```bash
/var/lib/fluxbee/blob
```

Primero, revisar si el nodo tiene override explicito:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"archi"}'
```

Si no ves config de blob, asumir default.

Inspeccion minima:

```bash
sudo find /var/lib/fluxbee/blob -maxdepth 3 -type d | sort
```

Para ver actividad reciente:

```bash
sudo find /var/lib/fluxbee/blob -type f -mmin -2 | sort
```

Si queres una vista mas amplia:

```bash
sudo find /var/lib/fluxbee/blob -type f | tail -n 50
```

Lectura esperada:

- deberian existir al menos directorios bajo `staging` y/o `active`
- despues de una prueba multipart reciente, deberia aparecer al menos un archivo nuevo por timestamp

Importante:

- que `cd /var/lib/fluxbee/blob` falle con `Permission denied` no implica error de materializacion
- que `find .../active -mmin -10` vuelva vacio tampoco alcanza para concluir falla:
  - el blob root efectivo podria ser otro
  - el archivo podria haber quedado fuera de esa ventana
  - el layout podria no coincidir con la expectativa del nombre buscado

Si queres una prueba mas fuerte, subir un attachment nuevo y enseguida correr:

```bash
sudo find /var/lib/fluxbee/blob -type f -mmin -1 | sort
```

---

## 18. Errores esperables y como leerlos

### `503 node_not_configured`

Significa:

- el proceso esta vivo;
- pero la instancia todavia no tiene config efectiva suficiente.

Accion:

- revisar `CONFIG_GET`
- reenviar `CONFIG_SET` valido

### `401 unauthorized`

Causas tipicas:

- falta header `Authorization`
- token invalido
- token no materializado o no cargado en runtime

### `415 unsupported_media_type`

Causas tipicas:

- el request llega con content type no admitido por `ingress.accepted_content_types`

### `422 invalid_payload`

Causas tipicas:

- falta `subject` en `explicit_subject`
- viene `subject` en `caller_is_subject`
- falta `message`
- falta `text` y tampoco hay attachments

### `413 payload_too_large`

Causas tipicas:

- body JSON supera `ingress.max_request_bytes`
- archivo supera `max_attachment_size_bytes`
- total supera `max_total_attachment_bytes`

### `415 unsupported_attachment_mime`

Causa:

- el attachment llega con MIME fuera de `ingress.allowed_mime_types`

---

## 19. Logs recomendados

Si queres seguir las pruebas en tiempo real:

```bash
sudo journalctl -u fluxbee-node-IO.api.frontdesk-motherbee -f
```

O filtrado:

```bash
sudo journalctl -u fluxbee-node-IO.api.frontdesk-motherbee --since "10 min ago" --no-pager | rg -n "io-api|relay|sent to router|failed to send to router"
```

Senales utiles que hoy existen:

- `io-api loaded spawn config`
- `io-api http listener ready`
- `io-api runtime config hot-applied`
- `relay session opened`
- `relay fragment buffered`
- `relay session flushed`
- `sent to router`

---

## 20. Confirmacion minima de exito

La validacion runtime minima de `IO.api` puede darse por suficiente si se cumple:

1. el runtime `IO.api` se publica y el `SYSTEM_UPDATE` queda `ok`;
2. el `SPAWN_NODE` arranca la instancia;
3. `GET /` responde y refleja el contrato efectivo;
4. `CONFIG_SET` acepta auth + ingress + relay;
5. `POST /` JSON responde `202`;
6. `POST /` multipart responde `202`;
7. el nodo muestra envio al router o hold/flush del relay segun la policy;
8. `CONFIG_GET` devuelve secretos redacted, no en claro.

---

## 21. Camino recomendado para la primera prueba

Si queres ir por el camino mas corto y menos ambiguo:

1. build release
2. publish con `publish-runtime.sh`
3. `SYSTEM_UPDATE`
4. spawn minimo con `listen.address` y `listen.port`
5. `CONFIG_SET` de `explicit_subject` con:
   - token inline
   - `application/json`
   - `multipart/form-data`
   - relay `window_ms = 300000` si queres validar hold por defecto, o `0` si queres forzar flush inmediato
6. probar `GET /`
7. probar `POST /` JSON
8. probar `POST /` multipart
9. opcional: cambiar relay a `window_ms = 2500`
10. repetir dos requests seguidos y revisar hold/flush

Ese orden reduce variables y deja para el final la prueba de consolidacion.

---

## 22. Helpers nuevos para `IO.api`

Durante esta implementacion se agregaron estos helpers:

- `scripts/publish-io-api-runtime.sh`
- `scripts/deploy-io-api.sh`
- `scripts/install-io-api.sh`

Uso recomendado de cada uno:

- `publish-io-api-runtime.sh`: publica el runtime `IO.api` y deja `RUST_LOG` default util para `io_api` e `io_common` dentro del `start.sh` publicado
- `deploy-io-api.sh`: hace `publish + update + spawn/restart` siguiendo el mismo patron operativo de otros IO
- `install-io-api.sh`: instala `io-api` como service local de desarrollo, crea `io-api.env` y bootstrap managed `config.json`

Ejemplo de publish:

```bash
bash scripts/publish-io-api-runtime.sh \
  --version "$VERSION" \
  --set-current \
  --sudo
```

Ejemplo de deploy:

```bash
bash scripts/deploy-io-api.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$VERSION" \
  --node-name "$NODE_NAME" \
  --tenant-id "$TENANT_ID" \
  --listen-address "$LISTEN_ADDRESS" \
  --listen-port "$LISTEN_PORT" \
  --sync-hint \
  --sudo
```

Ejemplo de update sobre nodo existente:

```bash
bash scripts/deploy-io-api.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$VERSION" \
  --node-name "$NODE_NAME" \
  --update-existing \
  --sync-hint \
  --sudo
```

Ejemplo de install local:

```bash
bash scripts/install-io-api.sh \
  --node-name "$NODE_NAME" \
  --listen-address "$LISTEN_ADDRESS" \
  --listen-port "$LISTEN_PORT"
```

Luego:

```bash
sudo systemctl enable --now fluxbee-io-api
sudo journalctl -u fluxbee-io-api -f
```

---

## 23. Nota de bootstrap minimo

`IO.api` no necesita config de negocio completa para arrancar el proceso.

Lectura correcta:

- puede bootear con bootstrap vacio o casi vacio;
- puede quedar vivo en `UNCONFIGURED` o `FAILED_CONFIG`;
- en ese estado no debe aceptar trafico de negocio y eso es correcto;
- la configuracion funcional de negocio sigue entrando por `CONFIG_SET`.

En particular:

- `listen.*` no debe leerse como requisito funcional para que el nodo "exista";
- en los helpers nuevos, `listen.address` y `listen.port` quedan como conveniencia opcional de bootstrap;
- si no se proveen, el runtime cae en sus defaults internos y la configuracion de negocio sigue siendo la que determina si el nodo realmente opera o no.

Regla practica:

- para pruebas controladas, suele convenir fijar `listen.*`;
- para respetar el modelo de nodo vivo pero no configurado, se puede bootstrapear sin config funcional.

---

## 24. Limites conocidos y backlog inmediato

Lo que hoy queda deliberadamente fuera o incompleto en `IO.api`:

- outbound webhook
- auth HMAC o mTLS
- `mapped_subject`
- mayor cobertura de tests HTTP/multipart/relay/blob
- observabilidad mas fina de algunos rechazos HTTP
- helper publish/deploy local validado en Linux todavia pendiente de ejecucion real

Tambien queda una deuda estructural acotada:

- el crate `io-api` sigue mayormente concentrado en `main.rs`
- eso no bloquea la funcionalidad actual, pero deja abierta una refactorizacion posterior a modulos si el nodo sigue creciendo

---

## 25. Prueba dirigida de `explicit_subject by_data`

Esta seccion deja una receta concreta para validar el flujo nuevo de `explicit_subject` cuando el sujeto llega por datos y el nodo debe:

- validar campos minimos;
- consultar/provisionar identity antes del send;
- devolver el `ilk` efectivo en la respuesta;
- rechazar payloads incompletos sin enviar al router.

### 25.1. Variables sugeridas

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
VERSION="0.1.0"
TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"

NODE_NAME="IO.api.support@$HIVE_ID"
LISTEN_ADDRESS="127.0.0.1"
LISTEN_PORT="18081"
API_KEY="fb_api_support_demo_001"
```

Nota:

- si vas a usar otro `NODE_NAME` y otro `LISTEN_PORT`, no hace falta borrar una instancia previa;
- si queres reutilizar el mismo nombre o puerto, borrar primero la instancia anterior.

### 25.2. Borrar un nodo previo si hace falta

Ejemplo, solo si queres liberar nombre o puerto:

```bash
OLD_NODE_NAME="IO.api.frontdesk@$HIVE_ID"

curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$OLD_NODE_NAME"
```

### 25.3. Publish + update + spawn del nodo nuevo

```bash
bash scripts/deploy-io-api.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$VERSION" \
  --node-name "$NODE_NAME" \
  --tenant-id "$TENANT_ID" \
  --listen-address "$LISTEN_ADDRESS" \
  --listen-port "$LISTEN_PORT" \
  --sync-hint \
  --sudo
```

El helper actual hace el flujo completo:

- `publish`
- `update`
- `spawn`
- `CONFIG_SET` con config efectiva
- `restart` del unit

### 25.4. Bloqueo operativo conocido: `SY.identity` / SHM desalineado

Durante la validacion real aparecio un caso operativo que no pertenece a `IO.api` sino al core:

- `IO.api` puede responder:
  - `503 identity_unavailable`
  - `Unable to validate authenticated API key tenant: invalid identity SHM header/layout`

Lectura correcta:

- el runtime activo de `SY.identity` en el host puede estar desalineado respecto del layout SHM esperado por el core / `fluxbee_sdk` del repo;
- en ese estado, la validacion de tenant contra identity falla antes de entrar al flujo de negocio normal de `IO.api`;
- no corresponde seguir depurando `IO.api` hasta alinear primero `SY.identity`.

Camino recomendado:

1. actualizar/reinstalar core (`scripts/install.sh`) para alinear `sy-identity` con el repo;
2. reiniciar `sy-identity.service`;
3. confirmar que desaparece el error `invalid identity SHM header/layout` antes de continuar con pruebas de `frontdesk`.

Comandos base:

```bash
cd /home/administrator/fluxbee-nodes/fluxbee
bash scripts/install.sh
sudo systemctl restart sy-identity.service
sudo systemctl is-active sy-identity.service
sudo journalctl -u sy-identity.service -n 80 --no-pager
```

Si ese error vuelve a aparecer, el bloqueo debe tratarse como dependencia externa / coordinación con core, no como bug funcional de `IO.api`.

### 25.4.b. Bloqueo operativo conocido: `sy-identity` activo pero no consumiendo trafico del router

Durante la validacion tambien aparecio un caso operativo distinto del problema de SHM:

- `IO.api` envia `ILK_PROVISION`;
- `rt-gateway` registra el mensaje hacia `SY.identity@motherbee`;
- `sy-identity.service` figura `active`;
- pero `sy-identity` no loguea:
  - `identity received ILK_PROVISION request`
  - ni `identity sending system response`;
- y `IO.api` termina devolviendo:
  - `504 identity_timeout`
  - o `503 identity_unavailable`.

Lectura correcta:

- el servicio puede estar vivo para systemd pero no operativo como consumidor efectivo del trafico del router;
- en ese estado, el problema no pertenece a la logica funcional de `IO.api`;
- tampoco corresponde leerlo como falla de SHM o de `frontdesk`;
- el sintoma tipico es:
  - `rt-gateway` ve `ILK_PROVISION`
  - pero no aparece `ILK_PROVISION_RESPONSE`
  - y `sy-identity` queda silencioso para ese `trace_id`.

Primer remedio operativo recomendado:

```bash
sudo systemctl restart sy-identity.service
sudo systemctl is-active sy-identity.service
sudo journalctl -u sy-identity.service -n 40 --no-pager -l
```

Si despues del restart los requests nuevos vuelven a responder `202`, tratar el incidente como problema operativo de `sy-identity` / core y no como regression de `IO.api`.

Si vuelve a repetirse el patron, juntar esta evidencia minima:

```bash
sudo journalctl -u sy-identity.service --since "2 min ago" --no-pager -l
sudo journalctl -u rt-gateway --since "2 min ago" --no-pager | rg -n "ILK_PROVISION|ILK_PROVISION_RESPONSE|SY.identity|node disconnected|hello received|node registered"
```

Con esa evidencia, el seguimiento corresponde al equipo de core.

### 25.5. Camino alternativo de validacion mientras identity/core esta en movimiento

Si el equipo de core esta tocando `SY.identity` y no conviene seguir persiguiendo el camino completo de regularizacion en ese momento, igual se puede validar parcialmente el comportamiento de `IO.api` con un sujeto ya existente.

Objetivo de esta variante:

- no validar `frontdesk` end-to-end desde cero;
- si validar que `IO.api`, cuando la identidad ya existe o ya fue completada, continua al `dst_final` correcto y devuelve `202 accepted`.

Lectura esperada:

1. primer request con sujeto nuevo:
   - puede gatillar `ILK_PROVISION` + `frontdesk`;
   - si hay un problema de correlacion o una carrera en runtime, puede devolver timeout aunque el efecto de negocio quede aplicado;
2. request repetido con el mismo sujeto ya regularizado:
   - ya no deberia entrar a `frontdesk`;
   - deberia continuar al `dst_final`;
   - la respuesta HTTP correcta pasa a ser `202 Accepted`.

Esto deja una evidencia util mientras identity/core no esta estable:

- la parte "si la identidad existe, seguir camino normal" queda validada;
- la parte "alta y regularizacion del primer request" queda temporalmente condicionada por estabilidad del core.

### 25.6. Estado observado en validacion Linux

Hasta este punto ya se observo en Linux:

- `frontdesk` puede completar `ILK_REGISTER` correctamente;
- un retry posterior sobre el mismo `external_user_id` puede devolver `202 Accepted`;
- eso confirma que, con identidad existente / completa, `IO.api` sigue el camino normal hacia `dst_final`;
- despues de reiniciar `sy-identity`, requests nuevos de alta tambien volvieron a responder `202 Accepted`;
- eso confirma que el camino `ILK_PROVISION -> frontdesk -> continue to dst_final` puede funcionar correctamente cuando `SY.identity` esta operativo.

Casos observados con `202 Accepted` despues de estabilizar `sy-identity`:

- `support:customer-frontdesk-007`
- `support:customer-frontdesk-009`
- `support:customer-frontdesk-010`

Lectura operativa:

- si el mismo branch / runtime de `IO.api` alterna entre `identity_timeout` y `202` segun el estado operativo de `sy-identity`, no corresponde atribuir ese comportamiento a un bug nuevo de `IO.api`;
- el criterio correcto es separar:
  - validacion funcional de `IO.api`, que ya quedo razonablemente cubierta;
  - incidencias operativas de `sy-identity`, que deben seguirse con core.

Lo que sigue abierto para terminar de validar el flujo completo no es la continuacion de sujetos existentes, sino la robustez del primer request cuando hay:

- `ILK_PROVISION`
- handoff a `SY.frontdesk.gov`
- respuesta inmediata de `frontdesk_result`
- y continuation al `dst_final`

Si el entorno muestra sintomas de carrera o desalineacion de core, priorizar:

1. estabilizar `SY.identity`;
2. repetir solo despues la validacion del primer request nuevo.

### 25.6.b. Estado de validacion funcional actual

Con la evidencia reunida hasta ahora, se puede dar por razonablemente validado del lado de `IO.api`:

1. el trigger de `frontdesk` ya no depende de `dst_node = SY.frontdesk.gov`;
2. `IO.api` puede resolver/provisionar identidad y continuar al `dst_final`;
3. `IO.api` devuelve `202 Accepted` cuando la identidad queda resuelta y el flujo sigue su camino;
4. los timeouts observados al final de esta tanda quedaron correlacionados con estado operativo de `sy-identity`, no con fallas sistematicas del flujo de `IO.api`.

Pruebas recomendadas para cerrar la tanda:

1. prueba positiva con evidencia de logs:
   - `IO.api` hace `ILK_PROVISION`
   - `SY.frontdesk.gov` hace `ILK_REGISTER`
   - el nodo AI destino recibe el mensaje
2. prueba sin `dst_node` explicito:
   - usar `resolve` o el default configurado
   - confirmar que el flujo no depende del destino overrideado por request
- `schema probe` sobre `http://$LISTEN_ADDRESS:$LISTEN_PORT/schema`

Endpoints HTTP canonicos actuales de `IO.api`:

- `GET /`
- `POST /`

Aliases legacy compatibles que pueden seguir usandose:

- `GET /schema`
- `POST /messages`

Defaults relevantes del helper actual:

- `auth.api_keys[0].key_id = support-main`
- `io.relay.window_ms = 300000`
- `io.relay.max_open_sessions = 10000`
- `io.relay.max_fragments_per_session = 8`
- `io.relay.max_bytes_per_session = 262144`

Si queres verificar que el helper construyo bien el bootstrap, revisar el log del deploy y confirmar lineas de este estilo:

```bash
step=spawn_config config={"listen":{"address":"127.0.0.1","port":18081}}
step=config_set config_version=... relay_window_ms=300000 dst_node=resolve
schema_probe_ok url=http://127.0.0.1:18081/schema attempt=1
```

Si en cambio aparece un warning tipo:

```bash
Warning: empty config payload for sanitize (source=stdin); using {}
```

entonces el spawn se hizo con config vacia y el nodo puede caer en defaults internos. La consecuencia tipica es que intente bindear `127.0.0.1:8080` y choque con otro proceso.

En ese caso, antes de seguir con `CONFIG_SET`, revisar:

```bash
sudo journalctl -u fluxbee-node-IO.api.support-motherbee --since "10 min ago" --no-pager
```

y confirmar que no haya:

```bash
failed to bind HTTP listener ... Address already in use
```

Comportamiento esperado del runtime actual:

- `IO.api` no deberia entrar en crash-loop por ese motivo;
- debe quedar vivo para `CONFIG_GET` / `CONFIG_SET`;
- y el estado debe reflejar `FAILED_CONFIG` con `last_error.code = listener_bind_failed`.

Nota sobre `CONFIG_SET` desde el helper:

- el endpoint puede devolver `TIMEOUT` aunque la config haya quedado aplicada;
- si despues del restart aparece `schema_probe_ok`, el deploy debe considerarse exitoso;
- la validacion de verdad es el `schema` efectivo, no la respuesta HTTP del `CONFIG_SET`.

### 25.4. `CONFIG_SET` manual para `explicit_subject`

Normalmente este paso ya no hace falta porque `deploy-io-api.sh` lo ejecuta automaticamente.

Usarlo solo si queres:

- cambiar la config sin redeploy,
- corregir un nodo ya spawneado,
- o forzar una config distinta a la default del helper.

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d "{
    \"schema_version\": 1,
    \"config_version\": 3,
    \"apply_mode\": \"replace\",
    \"config\": {
      \"listen\": {
        \"address\": \"$LISTEN_ADDRESS\",
        \"port\": $LISTEN_PORT
      },
      \"auth\": {
        \"mode\": \"api_key\",
        \"api_keys\": [
          {
            \"key_id\": \"support-main\",
            \"token\": \"$API_KEY\"
          }
        ]
      },
      \"ingress\": {
        \"subject_mode\": \"explicit_subject\",
        \"accepted_content_types\": [
          \"application/json\",
          \"multipart/form-data\"
        ],
        \"max_request_bytes\": 262144,
        \"max_attachments_per_request\": 4,
        \"max_attachment_size_bytes\": 10485760,
        \"max_total_attachment_bytes\": 20971520,
        \"allowed_mime_types\": [
          \"image/png\",
          \"image/jpeg\",
          \"application/pdf\",
          \"text/plain\",
          \"application/json\"
        ]
      },
      \"io\": {
        \"dst_node\": \"resolve\",
        \"relay\": {
          \"window_ms\": 300000,
          \"max_open_sessions\": 10000,
          \"max_fragments_per_session\": 8,
          \"max_bytes_per_session\": 262144
        }
      }
    }
  }"
```

### 25.5. Confirmar schema efectivo

```bash
curl -sS "http://$LISTEN_ADDRESS:$LISTEN_PORT/"
```

Esperado:

- `status = configured`
- `ingress.subject_mode = explicit_subject`
- `relay.effective.window_ms = 300000`

### 25.6. Prueba 1: crear o resolver sujeto por datos

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $API_KEY" \
  -d '{
    "subject": {
      "external_user_id": "support:customer-001",
      "display_name": "Juan Perez",
      "email": "juan@example.com",
      "company_name": "Acme Support",
      "phone": "+5491100000001"
    },
    "message": {
      "text": "Necesito ayuda con mi cuenta",
      "external_message_id": "support-msg-001"
    },
    "options": {
      "routing": {
        "dst_node": "resolve"
      },
      "metadata": {
        "conversation_id": "support-case-001"
      }
    }
  }'
```

Esperado:

- HTTP `202`
- body con `ilk`
- `relay_status = held` si `config.io.relay.window_ms > 0`
- `relay_status = flushed_immediately` solo si el relay esta deshabilitado (`window_ms = 0`) o si el fragmento fuerza flush
- el tenant efectivo sale de la API key configurada, no del body
- si `options.routing.dst_node` viene presente, ese request debe salir con ese destino en lugar del `config.io.dst_node` de la instancia

Lectura funcional:

- si el sujeto no existia, aca deberia provisionarse;
- si ya existia, deberia resolverse el mismo `ilk`.

### 25.7. Prueba 2: repetir con los mismos datos

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $API_KEY" \
  -d '{
    "subject": {
      "external_user_id": "support:customer-001",
      "display_name": "Juan Perez",
      "email": "juan@example.com",
      "company_name": "Acme Support",
      "phone": "+5491100000001"
    },
    "message": {
      "text": "Segundo mensaje del mismo usuario",
      "external_message_id": "support-msg-002"
    },
    "options": {
      "metadata": {
        "conversation_id": "support-case-001"
      }
    }
  }'
```

Validacion:

- comparar el `ilk` de esta respuesta con el de la prueba 1;
- deberian ser el mismo.

### 25.8. Prueba 3: payload incompleto

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $API_KEY" \
  -d '{
    "subject": {
      "external_user_id": "support:customer-002",
      "display_name": "Maria Gomez"
    },
    "message": {
      "text": "Payload incompleto",
      "external_message_id": "support-msg-003"
    }
  }'
```

Esperado:

- HTTP `422`
- `error_code = "subject_data_incomplete"`

### 25.9. Logs a revisar

```bash
sudo journalctl -u fluxbee-node-IO.api.support-motherbee --since "10 min ago" --no-pager | rg -n "runtime config hot-applied|explicit_subject|identity lookup|identity provisioned|subject_data_incomplete|sent to router|failed to send to router"
```

Lectura esperada:

- prueba 1:
  - `io-api explicit_subject identity lookup miss`
  - `io-api explicit_subject identity provisioned`
  - con `relay.window_ms > 0`, puede no aparecer `sent to router` en el request inicial porque el fragmento puede quedar `held`
- prueba 2:
  - `io-api explicit_subject identity lookup hit`
  - mismo criterio: puede quedar `held`
- prueba 3:
  - no deberia haber `sent to router`

Seguimiento en vivo:

```bash
sudo journalctl -u fluxbee-node-IO.api.support-motherbee -f
```

### 25.10. Que valida esta tanda

Si esta secuencia da lo esperado, queda validado que:

1. `explicit_subject by_data` pasa por identity antes del send;
2. el `ilk` efectivo vuelve en la respuesta HTTP;
3. el segundo request reutiliza identidad existente cuando corresponde;
4. un payload incompleto se rechaza antes de tocar router.

### 25.11. Prueba `by_ilk`

Caso feliz con ILK existente:

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $API_KEY" \
  -d '{
    "subject": {
      "ilk": "ilk:ya-existente"
    },
    "message": {
      "text": "Mensaje para sujeto ya resuelto",
      "external_message_id": "support-msg-by-ilk-001"
    }
  }'
```

Esperado:

- HTTP `202`
- body con `ilk = "ilk:ya-existente"`
- no debe intentar provision por datos

Caso reject con ILK inexistente:

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $API_KEY" \
  -d '{
    "subject": {
      "ilk": "ilk:no-existe"
    },
    "message": {
      "text": "Mensaje invalido por ILK inexistente"
    }
  }'
```

Esperado:

- HTTP `404`
- `error_code = "ilk_does_not_exist"`

### 25.12. Prueba E2E con regularizacion automatica por `SY.frontdesk.gov`

Esta prueba valida el flujo canonico:

- `IO.api`
- resuelve/provisiona `src_ilk`
- si el sujeto no esta completo, construye `frontdesk_handoff`
- envia a `SY.frontdesk.gov`
- espera `frontdesk_result`
- si `status = "ok"`, continua el mensaje original al `dst_final` y responde `202 Accepted`
- si `status = "needs_input"` o `error`, devuelve ese resultado estructurado por HTTP

Request de ejemplo:

```bash
curl -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $API_KEY" \
  -d '{
    "subject": {
      "external_user_id": "support:customer-frontdesk-001",
      "display_name": "Juan Perez",
      "email": "juan@example.com",
      "company_name": "Acme Support",
      "attributes": {
        "crm_customer_id": "crm-123"
      }
    },
    "message": {
      "text": "Necesito ayuda con mi alta"
    },
    "options": {
      "routing": {
        "dst_node": "AI.chat@motherbee"
      },
      "metadata": {
        "conversation_id": "frontdesk-case-001"
      }
    }
  }'
```

Resultado esperado:

- si frontdesk devuelve `needs_input` o `error`, no debe devolver `202 Accepted`
- en esos casos debe devolver directamente un `frontdesk_result`
- si `status = "needs_input"`, el status HTTP esperado es `422 Unprocessable Entity`
- si `status = "ok"`, el status HTTP esperado es `202 Accepted` y debe existir `trace_id` del mensaje continuado al `dst_final`

Shape esperado cuando frontdesk bloquea la continuacion (`needs_input` / `error`):

```json
{
  "type": "frontdesk_result",
  "schema_version": 1,
  "status": "needs_input|ok|error",
  "result_code": "...",
  "human_message": "...",
  "missing_fields": [],
  "error_code": null,
  "error_detail": null,
  "ilk_id": "ilk:...",
  "tenant_id": "tnt:...",
  "registration_status": "temporary|complete"
}
```

Shape esperado cuando frontdesk devuelve `ok`:

```json
{
  "status": "accepted",
  "request_id": "req_01JXYZ...",
  "trace_id": "4c66b54c-9d53-4b44-8eb0-c9f88f8f4c1e",
  "relay_status": "flushed_immediately",
  "node_name": "IO.api.support@motherbee",
  "ilk": "ilk:..."
}
```

Logs a revisar:

`IO.api`

```bash
sudo journalctl -u fluxbee-node-IO.api.support-motherbee --since "10 min ago" --no-pager | rg -n "frontdesk|pending http request|delivered router reply|invalid_frontdesk_response|frontdesk_timeout"
```

`SY.frontdesk.gov`

```bash
sudo journalctl -u sy-frontdesk-gov.service --since "10 min ago" --no-pager | rg -n "frontdesk_handoff|frontdesk_result|ilk_register|needs_input|REGISTERED|MISSING_REQUIRED_FIELDS"
```

Que valida esta prueba:

1. `IO.api` decide regularizacion por estado del sujeto y no por `dst=frontdesk`
2. `IO.api` construye `frontdesk_handoff`
3. `SY.frontdesk.gov` responde con `frontdesk_result`
4. `IO.api` correlaciona la reply por `trace_id`
5. si frontdesk devuelve `ok`, el mensaje original continua al `dst_final`
6. si frontdesk devuelve `needs_input` o `error`, el resultado estructurado vuelve correctamente por HTTP

### 25.13. Estado real de `needs_input` con el contrato actual de `IO.api`

Aunque `SY.frontdesk.gov` implementa `frontdesk_result.status = "needs_input"`, ese estado no es hoy un caso E2E alcanzable desde el contrato HTTP vigente de `IO.api`.

Motivo:

- `IO.api` exige `subject.display_name` y `subject.email` antes de construir el `frontdesk_handoff`;
- `SY.frontdesk.gov` devuelve `needs_input` justamente cuando faltan `name` o `email` en el handoff estructurado.

Consecuencia:

- un request HTTP sin `display_name` o sin `email` no llega a `SY.frontdesk.gov`;
- queda rechazado antes en `IO.api` como:
  - `422 Unprocessable Entity`
  - `error_code = "subject_data_incomplete"`.

Lectura correcta para validacion:

- `needs_input` sigue siendo parte del contrato formal de `SY.frontdesk.gov`;
- pero no debe exigirse como prueba E2E de `IO.api` mientras el contrato actual de `explicit_subject by_data` requiera esos mismos campos minimos;
- el rechazo correcto desde `IO.api` para ese caso es `subject_data_incomplete`.

Estado observado en Linux:

- request sin `subject.email`:
  - HTTP `422`
  - `error_code = "subject_data_incomplete"`
  - sin handoff a frontdesk

Por lo tanto, la validacion funcional de `IO.api` debe considerarse cerrada con:

1. `lookup hit` reutilizando identidad existente;
2. `lookup miss -> ILK_PROVISION -> frontdesk -> continue -> 202`;
3. rechazo temprano `subject_data_incomplete` para payload incompleto;
4. flush posterior del relay y entrega al router hacia el `dst_final`.

### 25.14. Validacion tenant-aware real por API key

Con el contrato tenant-scoped actual, la validacion importante no es solo que `IO.api` llegue a frontdesk, sino que no colisione identidad entre tenants cuando se repite el mismo `(channel, external_user_id)`.

Resultado validado en Linux:

- tenant A:
  - API key asociada a `tnt:43d576a3-d712-4d91-9245-5d5463dd693e`
  - `external_user_id = support:tenant-split-001`
  - `ilk = ilk:ee207fb9-e3cb-4703-b648-711a8b656692`
- tenant B:
  - API key asociada a `tnt:3436cce7-c1ba-407a-add1-e6322add4e39`
  - mismo `external_user_id = support:tenant-split-001`
  - `ilk = ilk:24c38a6c-2265-4298-8c44-6748af85c5cf`

Lectura correcta:

- el mismo `external_user_id` ya no colisiona entre tenants;
- `IO.api` y `io-common` resuelven/provisionan por `(tenant_id, channel, external_user_id)`;
- el aislamiento multitenant real del lado IO quedo validado.

Validacion adicional ejecutada:

- tenant B, sujeto nuevo `support:tenant-b-new-002`:
  - primer request:
    - `lookup miss`
    - `identity provisioned`
    - `ilk = ilk:d66c6b25-b0a8-4ff5-9f5b-2cfb1baa7f2f`
  - segundo request:
    - `lookup hit`
    - mismo `ilk = ilk:d66c6b25-b0a8-4ff5-9f5b-2cfb1baa7f2f`
    - `registration_status = complete`

Eso cierra:

1. lookup tenant-aware;
2. provision tenant-aware;
3. no colision cross-tenant;
4. reutilizacion correcta del mismo ILK dentro del tenant.

### 25.15. Incidencia operativa observada en `SY.frontdesk.gov`

Durante la validacion tenant-aware aparecio este patron:

- `sy-frontdesk-gov.service` figuraba `active` en systemd;
- pero `rt-gateway` devolvia `UNREACHABLE` con:
  - `reason = "NODE_NOT_FOUND"`
  - `original_dst = "SY.frontdesk.gov@motherbee"`
- `IO.api` lo exponia como:
  - `502 Bad Gateway`
  - `error_code = "invalid_frontdesk_response"`

Lectura correcta:

- no era un bug funcional nuevo de `IO.api`;
- `SY.frontdesk.gov` estaba vivo para systemd pero no registrado efectivamente en el router.

Remedio operativo validado:

```bash
sudo systemctl restart sy-frontdesk-gov.service
sudo journalctl -u rt-gateway --since "5 min ago" --no-pager | rg -n "SY.frontdesk.gov@motherbee|hello received|node registered"
```

Se considera recuperado cuando `rt-gateway` vuelve a mostrar:

- `hello received`
- `node registered ... name=SY.frontdesk.gov@motherbee`

Despues de ese reinicio, las pruebas tenant-aware volvieron a responder `202 Accepted`.
