# 4iPlatform Support Rollout - Plan de tareas y lineamientos

**Estado:** plan de ejecución operativo  
**Fecha:** 2026-04-21  
**Base de trabajo:**
- `docs/02-protocolo.md`
- `docs/07-operaciones.md`
- `docs/AI_nodes_spec.md`
- `docs/ai-nodes-deploy-runbook.md`
- `docs/io/io-api-node-spec.md`
- `docs/io/io-api-http-contract-examples.md`
- `docs/io/io-api-webhook-outbound-spec.md`
- `docs/io/io-common.md`
- `docs/onworking NOE/io-api-explicit-subject-identity-tasks.md`
- `docs/onworking NOE/io_api_webhook_outbound_implementation_tasks.md`

---

## 0. Objetivo

Resolver en un mismo frente operativo:

1. actualización de runtimes ya instalados (`IO.slack`, `SY.frontdesk.gov`, `AI.*`);
2. release y spawn de un nuevo nodo `AI.support.rep`;
3. preparación de `IO.api` para recibir requests desde un servicio .NET que reenvía mensajes de WhatsApp;
4. definición del contrato exacto del request HTTP que ese servicio debe enviar;
5. resolución de tenant + API key para una integración nueva identificada como `4iPlatform`.

---

## 1. Decisiones cerradas y restricciones reales

### 1.1 `IO.api` para este caso

- El servicio .NET entra por `IO.api`.
- El tenant efectivo del request sale de la API key autenticada.
- El request **no** debe enviar:
  - `subject.tenant_id`
  - `subject.tenant_hint`
- El modo correcto para este caso es:
  - `ingress.subject_mode = "explicit_subject"`

### 1.2 Datos de persona requeridos

Para `explicit_subject by_data`, hoy el contrato exige como mínimo:

- `subject.external_user_id`
- `subject.display_name`
- `subject.email`

Además pueden enviarse:

- `subject.company_name`
- `subject.phone`
- `subject.attributes`

Importante:

- `company_name` es metadata humana/comercial, **no** tenant;
- `phone` es metadata del sujeto, **no** crea automáticamente un `ICH` ni registra canal WhatsApp en identity;
- el `channel` efectivo de este flujo sigue siendo `api`, porque el ingreso real se hace por `IO.api`.

### 1.3 Tenant `4iPlatform`

El nombre humano `4iPlatform` no reemplaza el `tenant_id` canónico.

Regla real del repo:

- `tenant_id` debe ser `tnt:<uuid-v4>`;
- `4iPlatform` puede ser el nombre del tenant en un flujo `TNT_CREATE`, pero el sistema opera con `tenant_id`.

### 1.4 Creación de tenant: camino correcto

El camino correcto no es “inventar” un `tenant_id` en `IO.api`.

Hechos del repo:

- `IO.api` valida fail-closed que el tenant de la API key exista;
- `TNT_CREATE` / `TNT_APPROVE` no están expuestos como mutación pública canónica de admin REST;
- las escrituras de identity son system-calls a `SY.identity`.

Conclusión:

- si el tenant no existe, primero hay que crearlo por el camino de identity correcto;
- recién después conviene configurar la API key de `IO.api`.

### 1.5 Nodo `AI.support.rep`

Camino actual soportado por el repo:

- runtime base: `AI.common`
- nodo concreto: `AI.support.rep@<hive>`
- prompt/comportamiento del nodo: vía config del nodo (`config.json`/`CONFIG_SET`)

Nota:

- el modelo de package `config_only` existe como dirección documental, pero el camino operativo vigente del repo hoy es `runtime AI.common + config del nodo`.

### 1.6 Webhook outbound

Para la integración base .NET, el webhook outbound no era requisito inicial.

Durante la validación operativa de este rollout sí se habilitó un callback saliente de prueba para verificar:

- entrega final outbound desde `IO.api`;
- contrato HTTP externo del callback;
- firma HMAC correcta usando secreto compartido;
- compatibilidad con un receptor local expuesto por `ngrok`.

Entonces hay dos estados válidos para esta integración:

1. operativo base:
   - `final_reply_required = false`
   - sin callback final obligatorio
2. modo prueba / validación webhook:
   - `final_reply_required = true`
   - `webhook.enabled = true`
   - callback firmado por HMAC

Sigue siendo recomendable modelar una `integration_id`, porque el contrato actual de `IO.api` ya separa:

- `auth.api_keys[*]`
- `integrations[*]`

---

## 2. Orden recomendado de ejecución

1. inventario actual de runtimes y nodos instalados;
2. actualización de runtimes existentes;
3. definición/config de `AI.support.rep`;
4. creación o resolución del tenant `4iPlatform`;
5. configuración de `IO.api` con API key + integración sin webhook;
6. definición y validación del request .NET;
7. prueba E2E.

---

## 3. Fase A - Inventario y actualización de runtimes

### A.1 Inventario

- [ ] A1. Listar nodos actuales del hive objetivo.
- [ ] A2. Identificar:
  - `IO.slack`
  - `SY.frontdesk.gov`
  - nodos `AI.*` ya instalados
  - runtime/version de cada uno
- [ ] A3. Identificar si `SY.frontdesk.gov` está bajo orchestrator o bajo servicio singleton local.

Comandos orientativos:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"

curl -sS "$BASE/hives/$HIVE_ID/nodes" | jq .
curl -sS "$BASE/hives/$HIVE_ID/versions" | jq .
```

Si frontdesk está como servicio singleton local, verificar además:

```bash
systemctl cat sy-frontdesk-gov.service
systemctl show -p MainPID --value sy-frontdesk-gov.service
```

### A.2 Actualización de `AI.common`

- [ ] A4. Publicar/updatear runtime `AI.common`.
- [ ] A5. Reaplicar/updatear nodos `AI.*` instalados que dependan de `AI.common`.

Comando base:

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "<VERSION>" \
  --sync-hint \
  --sudo
```

Para un nodo ya existente:

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "<VERSION>" \
  --node-name "<AI.NODE@HIVE>" \
  --update-existing \
  --sync-hint \
  --sudo
```
--------------------
Comandos para actualizar los nodos existentes: 
AI.chat
```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"

bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "0.1.3" \
  --node-name "AI.chat@motherbee" \
  --update-existing \
  --sync-hint \
  --sudo
```
IO.Slack
```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "1.1.4-t126" \
  --node-name "IO.slack.T126@motherbee" \
  --update-existing \
  --sync-hint \
  --sudo

```
IO.Api
```bash
bash scripts/deploy-io-api.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "0.1.1" \
  --node-name "IO.api.support@motherbee" \
  --update-existing \
  --sync-hint \
  --sudo \
  --skip-config-set

```
--------------------
### A.3 Actualización de `IO.slack`

- [ ] A6. Publicar/updatear runtime `IO.slack`.
- [ ] A7. Reaplicar/updatear el/los nodos Slack existentes.

Comando base:

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "<VERSION>" \
  --node-name "<IO.SLACK@HIVE>" \
  --update-existing \
  --sync-hint \
  --sudo
```

### A.4 Actualización de `SY.frontdesk.gov`

- [ ] A8. Actualizar `SY.frontdesk.gov`.
- [ ] A9. Validar que el proceso real activo quedó con el binario/runtime esperado.

Camino correcto esperado para este rollout:

 version depende del runtime string que quiera publicar. para ver las versiones disponibles:
 `curl -sS "$BASE/hives/$HIVE_ID/runtimes/SY.frontdesk.gov" | jq .`

```bash
FD_VERSION="1.0.0"
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "SY.frontdesk.gov" \
  --version "$FD_VERSION" \
  --sync-hint \
  --sudo
```

Luego reiniciar el servicio singleton real:

```bash
sudo systemctl restart sy-frontdesk-gov.service
sudo systemctl status sy-frontdesk-gov.service --no-pager -l
```

Y validar que el nodo volvió a registrar correctamente:

```bash
curl -sS "$BASE/hives/$HIVE_ID/nodes" | jq '.payload.nodes[] | select(.node_name=="SY.frontdesk.gov@motherbee")'
```

Importante:

- para updates rutinarios, `SY.frontdesk.gov` debe tratarse como singleton canónico por hive;
- no conviene usar `--node-name ... --update-existing` como si fuera un `AI.*` / `IO.*` común;
- delete + spawn limpio quedan reservados para reinstalación deliberada, validación de bootstrap o recuperación de estado roto.
- en este host puntual, `publish + SYSTEM_UPDATE + restart` no alcanza por sí solo para cambiar el binario efectivo del servicio, porque `sy-frontdesk-gov.service` ejecuta `/usr/bin/sy-frontdesk-gov`.

Camino real completo cuando el servicio singleton usa `/usr/bin/sy-frontdesk-gov`:

1. publicar runtime `SY.frontdesk.gov` en `dist` para mantener el circuito de runtime/manifiesto alineado;
2. ejecutar `SYSTEM_UPDATE` targeted para `SY.frontdesk.gov`;
3. compilar el binario real del servicio:

```bash
cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
```

4. instalar ese binario en el path que usa systemd:

```bash
sudo install -m 0755 target/release/sy-frontdesk-gov /usr/bin/sy-frontdesk-gov
```

5. reiniciar el servicio singleton:

```bash
sudo systemctl restart sy-frontdesk-gov.service
sudo systemctl status sy-frontdesk-gov.service --no-pager -l
```

6. validar registro del nodo y alineación del binario:

```bash
curl -sS "$BASE/hives/$HIVE_ID/nodes" | jq '.payload.nodes[] | select(.node_name=="SY.frontdesk.gov@motherbee")'

sha256sum /usr/bin/sy-frontdesk-gov
sha256sum "/var/lib/fluxbee/dist/runtimes/SY.frontdesk.gov/$FD_VERSION/bin/sy-frontdesk-gov"
```

Si los hashes no coinciden, el servicio no quedó corriendo el mismo artefacto que el runtime publicado.

Alternativa mas amplia:

- si ademas queres refrescar el espejo local de core en `dist/core/bin` y dejar el host alineado por el camino de instalacion del repo, usar `scripts/install.sh`;
- ese camino es mas invasivo porque reinstala varios binarios/core services, no solo frontdesk.

---

## 4. Fase B - Release y spawn de `AI.support.rep`

### B.1 Naming y runtime

- [ ] B1. Definir hive destino.
- [ ] B2. Fijar nombre del nodo:
  - recomendado: `AI.support.rep@<hive>`
- [ ] B3. Confirmar que corre sobre runtime `AI.common`.

### B.2 Config funcional del nodo

- [ ] B4. Redactar prompt/instructions del support rep.
- [ ] B5. Elegir modelo y settings.
- [ ] B6. Definir `tenant_id` del nodo para spawn.

Ejemplo base de config JSON:

```json
{
  "behavior": {
    "kind": "openai_chat",
    "model": "gpt-4.1-mini",
    "instructions": {
      "source": "inline",
      "value": "PROMPT ESPECIFICO DEL AI.support.rep",
      "trim": true
    },
    "api_key_env": "OPENAI_API_KEY",
    "model_settings": {
      "temperature": 0.2,
      "top_p": 1.0,
      "max_output_tokens": 600
    }
  },
  "runtime": {
    "handler_timeout_ms": 120000,
    "worker_pool_size": 4,
    "queue_capacity": 128,
    "immediate_memory": {
      "enabled": true,
      "recent_interactions_max": 10,
      "active_operations_max": 8,
      "summary_max_chars": 1600,
      "summary_refresh_every_turns": 3,
      "trim_noise_enabled": true
    }
  }
}
```

### B.3 Spawn/update del nodo

- [ ] B7. Publicar/updatear `AI.common` si todavía no se hizo.
- [ ] B8. Crear archivo `config-json` del nodo.
- [ ] B9. Ejecutar spawn o update.

Helper inline recomendado para generar el config con prompt multilinea y API key inline:

```bash
PROMPT_FILE="$HOME/ai_support_rep.prompt.txt"
CONFIG_JSON="/tmp/AI.support.rep.config.json"
OPENAI_API_KEY="sk-..."

mkdir -p "$(dirname "$CONFIG_JSON")"
: > "$CONFIG_JSON"

PROMPT_FILE="$PROMPT_FILE" \
CONFIG_JSON="$CONFIG_JSON" \
TENANT_ID="$TENANT_ID" \
OPENAI_API_KEY="$OPENAI_API_KEY" \
python3 - <<'PY'
import json
import os
from pathlib import Path

prompt = Path(os.environ["PROMPT_FILE"]).read_text(encoding="utf-8")
doc = {
    "tenant_id": os.environ["TENANT_ID"],
    "behavior": {
        "kind": "openai_chat",
        "model": "gpt-4.1-mini",
        "instructions": {
            "source": "inline",
            "value": prompt,
            "trim": True,
        },
        "model_settings": {
            "temperature": 0.2,
            "top_p": 1.0,
            "max_output_tokens": 600,
        },
    },
    "runtime": {
        "handler_timeout_ms": 120000,
        "worker_pool_size": 4,
        "queue_capacity": 128,
        "immediate_memory": {
            "enabled": True,
            "recent_interactions_max": 10,
            "active_operations_max": 8,
            "summary_max_chars": 1600,
            "summary_refresh_every_turns": 3,
            "trim_noise_enabled": True,
        },
    },
    "secrets": {
        "openai": {
            "api_key": os.environ["OPENAI_API_KEY"],
        }
    },
}

Path(os.environ["CONFIG_JSON"]).write_text(
    json.dumps(doc, ensure_ascii=False, indent=2) + "\n",
    encoding="utf-8",
)
PY

jq . "$CONFIG_JSON"
```

Notas:

- `PROMPT_FILE` puede ser un `.txt` multilinea normal.
- El bloque Python serializa el prompt tal cual al JSON, sin que tengas que escapar saltos de línea.
- La key queda en el campo canónico `config.secrets.openai.api_key`.
- Si querés cambiar modelo o settings, editá los valores dentro del bloque JSON generado.

Comando base:

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "<VERSION>" \
  --node-name "AI.support.rep@$HIVE_ID" \
  --tenant-id "<TENANT_ID>" \
  --config-json "$CONFIG_JSON" \
  --spawn \
  --sync-hint \
  --sudo
```

### B.4 Validación

- [ ] B10. Verificar spawn exitoso.
- [ ] B11. Verificar `STATUS/CONFIG_GET`.
- [ ] B12. Hacer una prueba simple al nodo.

---

## 5. Fase C - Tenant `4iPlatform`

### C.1 Decisión de tenancy

- [ ] C1. Confirmar si ya existe un tenant Fluxbee para esta integración.
- [ ] C2. Si no existe, crear tenant nuevo.

### C.2 Regla de naming

Formato correcto:

- `tenant_id`: `tnt:<uuid-v4>`
- nombre humano deseado: `4iPlatform`

### C.3 Camino correcto si no existe

Camino correcto según repo:

- `TNT_CREATE` hacia `SY.identity`
- luego `TNT_APPROVE` si la política del entorno lo requiere

Payload conceptual de `TNT_CREATE`:

```json
{
  "name": "4iPlatform",
  "status": "pending"
}
```

Respuesta esperada:

```json
{
  "status": "ok",
  "tenant_id": "tnt:550e8400-e29b-41d4-a716-446655440000"
}
```

### C.4 Observación importante

No conviene avanzar con una API key en `IO.api` hasta tener resuelto un `tenant_id` real, porque `IO.api` valida fail-closed la existencia del tenant de la key.

---

## 6. Fase D - Configuración de `IO.api` para 4iPlatform

### D.1 Modelo a usar

Modelo operativo base:

- `final_reply_required = false`
- `integration_id` igualmente presente

Modelo de prueba webhook outbound:

- `final_reply_required = true`
- `webhook.enabled = true`
- `webhook.url` apuntando al receptor temporal
- `webhook.secret` o `webhook.secret_ref` según la versión efectiva del runtime

Recomendado:

- `integration_id = "int_4iplatform_support"`
- `key_id = "4iplatform-main"`

Decisión operativa para este rollout:

- si el tenant dedicado `4iPlatform` todavía no existe/aprueba, usar el tenant real ya disponible del entorno:
  - `tnt:43d576a3-d712-4d91-9245-5d5463dd693e`
- cuando exista el tenant dedicado `4iPlatform`, mover la API key y la integración a ese tenant sin cambiar el contrato HTTP del servicio .NET.

### D.2 Decisiones de routing

Dos opciones válidas:

1. fijar `io.dst_node = "AI.support.rep@<hive>"` en la config de `IO.api`;
2. dejar `io.dst_node = "resolve"` y mandar `options.routing.dst_node` por request.

Recomendación para este rollout:

- usar `options.routing.dst_node = "AI.support.rep@<hive>"` en las primeras pruebas;
- recién después, si queda estable, decidir si se fija como `dst_node` por instancia.

### D.3 Config JSON recomendada de `IO.api`

```json
{
  "listen": {
    "address": "127.0.0.1",
    "port": 18081
  },
  "auth": {
    "mode": "api_key",
    "api_keys": [
      {
        "key_id": "4iplatform-main",
        "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
        "integration_id": "int_4iplatform_support",
        "token": "fb_api_4iplatform_001"
      }
    ]
  },
  "integrations": [
    {
      "integration_id": "int_4iplatform_support",
      "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
      "final_reply_required": false,
      "webhook": {
        "enabled": false
      }
    }
  ],
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
```

### D.4 Aplicación

- [ ] D1. Configurar `IO.api` con API key + integración.
- [ ] D2. Verificar `GET /` de la instancia.
- [ ] D3. Guardar la API key para el servicio .NET.

Para `CONFIG_SET`, usar un `config_version` monotónico basado en epoch ms del momento actual:

```bash
CONFIG_VERSION="$(date +%s%3N)"
```

Si el entorno no soporta `date +%s%3N`:

```bash
CONFIG_VERSION="$(python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
)"
```

Importante:

- en `IO.api`, el `config_version` válido para `CONFIG_SET` no debe tomarse de `GET /nodes/{name}/status` ni de `GET /nodes/{name}/config`, porque esos endpoints pueden reflejar solo la capa persistida del nodo;
- el valor correcto es el `current_config_version` de la capa runtime/control-plane;
- la fuente correcta para leerlo es `POST /hives/{hive}/nodes/{node}/control/config-get`;
- si `control/config-get` timeouta, usar el `current_config_version` que aparezca en los logs del nodo y enviar `current + 1`.

Chequeo recomendado antes de aplicar `CONFIG_SET`:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"manual-rollout"}' | jq .
```

Si ese endpoint no responde a tiempo, fallback operativo:

```bash
sudo journalctl -u fluxbee-node-IO.api.support-motherbee.service --since "10 min ago" --no-pager \
  | rg "CONFIG_GET served|stale config_version|CONFIG_SET applied"
```

Ejemplo recomendado de `CONFIG_SET`:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="IO.api.support@motherbee"

LISTEN_ADDRESS="127.0.0.1"
LISTEN_PORT="18081"

TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
API_KEY_ID="4iplatform-main"
INTEGRATION_ID="int_4iplatform_support"
API_KEY="fb_api_4iplatform_001"

CONFIG_VERSION="1777000000006"

curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d "{
    \"requested_by\": \"manual-rollout\",
    \"schema_version\": 1,
    \"config_version\": $CONFIG_VERSION,
    \"apply_mode\": \"replace\",
    \"config\": {
      \"listen\": {
        \"address\": \"$LISTEN_ADDRESS\",
        \"port\": $LISTEN_PORT
      },
      \"tenant_id\": \"$TENANT_ID\",
      \"auth\": {
        \"mode\": \"api_key\",
        \"api_keys\": [
          {
            \"key_id\": \"$API_KEY_ID\",
            \"tenant_id\": \"$TENANT_ID\",
            \"integration_id\": \"$INTEGRATION_ID\",
            \"token\": \"$API_KEY\"
          }
        ]
      },
      \"integrations\": [
        {
          \"integration_id\": \"$INTEGRATION_ID\",
          \"tenant_id\": \"$TENANT_ID\",
          \"final_reply_required\": false,
          \"webhook\": {
            \"enabled\": false
          }
        }
      ],
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
  }" | jq .
```

### D.5 Validación específica de webhook outbound

Prueba realizada y validada en local:

- receptor HTTP local simple en `local-playground/webhook-outbound-test/webhook_receiver.py`;
- exposición temporal vía `ngrok`;
- `IO.api.support@motherbee` actualizado a runtime `0.1.5`;
- `CONFIG_SET` con:
  - `final_reply_required = true`
  - `webhook.enabled = true`
  - `integration_id = "int_4iplatform_support"`
  - `webhook.secret = "abc123"`
- callback recibido correctamente en el receptor local;
- firma HMAC validada correctamente (`provided == expected`, `valid = true`).

Headers observados correctamente en el callback:

- `X-Fluxbee-Delivery-Id`
- `X-Fluxbee-Timestamp`
- `X-Fluxbee-Signature`

Shape observado correctamente en el body:

- `schema_version`
- `delivery_id`
- `event_type = "io.api.final"`
- `integration_id`
- `tenant_id`
- `request`
- `result`
- `source`

Nota operativa:

- para acelerar la validación local se usó temporalmente `io.relay.window_ms = 30`;
- para operación normal ese valor debe volver a `300000`.

---

## 7. Fase E - Contrato HTTP para el servicio .NET

### E.1 Reglas

Sin adjuntos binarios:

```http
POST /
Authorization: Bearer <API_KEY>
Content-Type: application/json
```

Con adjuntos binarios:

```http
POST /
Authorization: Bearer <API_KEY>
Content-Type: multipart/form-data
```

Reglas:

- el tenant no viaja en el body; lo determina la API key configurada en `IO.api`;
- usar `application/json` solo cuando no haya binarios;
- usar `multipart/form-data` cuando haya uno o más adjuntos;
- en multipart, la parte `metadata` lleva el JSON canónico y las demás partes son archivos.

### E.2 Shape recomendado

```json
{
  "subject": {
    "external_user_id": "wa:+5491155551234",
    "display_name": "Juan Perez",
    "email": "juan@empresa.com",
    "company_name": "Empresa SA",
    "phone": "+5491155551234",
    "attributes": {
      "source_channel": "whatsapp",
      "provider": "4iplatform",
      "provider_user_id": "whatsapp-user-123",
      "provider_chat_id": "whatsapp-chat-456"
    }
  },
  "message": {
    "text": "Hola, necesito ayuda con mi pedido",
    "external_message_id": "wamid.HBgL....",
    "timestamp": "2026-04-21T12:34:56Z"
  },
  "options": {
    "routing": {
      "dst_node": "AI.support.rep@motherbee"
    },
    "metadata": {
      "source_system": "4iplatform-whatsapp-bridge",
      "source_channel": "whatsapp"
    }
  }
}
```

### E.3 Mapeo de campos

| Dato del servicio | Campo `IO.api` | Obligatorio | Observación |
|---|---|---:|---|
| Identificador estable de la persona | `subject.external_user_id` | Sí | Recomendado: prefijo estable tipo `wa:<e164>` o un ID de proveedor equivalente |
| Nombre | `subject.display_name` | Sí | Requerido en `by_data` |
| Mail | `subject.email` | Sí | Requerido en `by_data` |
| Compañía | `subject.company_name` | No | Metadata humana; no define tenant |
| Teléfono | `subject.phone` | No | Metadata; no crea ICH |
| Texto del mensaje | `message.text` | Sí* | Obligatorio salvo que el request sea solo con attachments |
| ID del mensaje externo | `message.external_message_id` | Recomendado | Útil para dedup/trazabilidad |
| Timestamp externo | `message.timestamp` | Recomendado | Formato RFC3339 |

### E.4 Campos inválidos para este caso

No enviar:

- `subject.tenant_id`
- `subject.tenant_hint`
- `webhook_url`
- `webhook_secret`

### E.5 Nota de identidad importante

Aunque el mensaje venga “desde WhatsApp” a nivel negocio:

- el canal real del adapter es `api`;
- identity resuelve/provisiona por `(tenant, channel=api, external_user_id)`;
- `phone` no registra automáticamente un canal WhatsApp en identity.

Entonces el valor de `external_user_id` debe ser:

- estable
- único dentro de la integración
- preferentemente derivado del identificador más estable que tengas del usuario en el sistema emisor

### E.6 Ejemplo .NET (shape)

Ejemplo mínimo con `HttpClient`:

```csharp
var body = new
{
    subject = new
    {
        external_user_id = "wa:+5491155551234",
        display_name = "Juan Perez",
        email = "juan@empresa.com",
        company_name = "Empresa SA",
        phone = "+5491155551234",
        attributes = new
        {
            source_channel = "whatsapp",
            provider = "4iplatform",
            provider_user_id = "whatsapp-user-123"
        }
    },
    message = new
    {
        text = "Hola, necesito ayuda con mi pedido",
        external_message_id = "wamid.HBgL....",
        timestamp = DateTimeOffset.UtcNow.ToString("O")
    },
    options = new
    {
        routing = new
        {
            dst_node = "AI.support.rep@motherbee"
        },
        metadata = new
        {
            source_system = "4iplatform-whatsapp-bridge",
            source_channel = "whatsapp"
        }
    }
};
```

### E.7 Ejemplo .NET con attachments

Cuando haya adjuntos, el servicio .NET debe enviar `multipart/form-data`.

Shape:

- parte `metadata`: JSON UTF-8 con el mismo contrato del ejemplo anterior
- una o más partes archivo

Ejemplo:

```csharp
using System.Net.Http.Headers;
using System.Text;

using var client = new HttpClient();
client.DefaultRequestHeaders.Authorization =
    new AuthenticationHeaderValue("Bearer", apiKey);

var metadata = """
{
  "subject": {
    "external_user_id": "wa:+5491155551234",
    "display_name": "Juan Perez",
    "email": "juan@empresa.com",
    "company_name": "Empresa SA",
    "phone": "+5491155551234",
    "attributes": {
      "source_channel": "whatsapp",
      "provider": "4iplatform",
      "provider_user_id": "whatsapp-user-123",
      "provider_chat_id": "whatsapp-chat-456"
    }
  },
  "message": {
    "text": "Adjunto documentacion",
    "external_message_id": "wamid.HBgL....",
    "timestamp": "2026-04-21T12:34:56Z"
  },
  "options": {
    "routing": {
      "dst_node": "AI.support.rep@motherbee"
    },
    "metadata": {
      "source_system": "4iplatform-whatsapp-bridge",
      "source_channel": "whatsapp"
    }
  }
}
""";

using var form = new MultipartFormDataContent();
form.Add(
    new StringContent(metadata, Encoding.UTF8, "application/json"),
    "metadata"
);

var pdfBytes = await File.ReadAllBytesAsync("/path/documentacion.pdf");
var pdf = new ByteArrayContent(pdfBytes);
pdf.Headers.ContentType = new MediaTypeHeaderValue("application/pdf");
form.Add(pdf, "file1", "documentacion.pdf");

var imageBytes = await File.ReadAllBytesAsync("/path/captura.png");
var image = new ByteArrayContent(imageBytes);
image.Headers.ContentType = new MediaTypeHeaderValue("image/png");
form.Add(image, "file2", "captura.png");

var response = await client.PostAsync("http://127.0.0.1:18081/", form);
```

Observaciones:

- los nombres `file1`, `file2`, etc. no son semánticos para `IO.api`; cualquier parte distinta de `metadata` se trata como attachment;
- no hace falta fabricar `attachments[]` del lado .NET;
- `IO.api` valida MIME/tamaño/cantidad, materializa blobs y arma el `text/v1` interno;
- si el request trae solo adjuntos, igual conviene mandar un `message.text` corto que dé contexto humano;
- para attachments binarios, este es el camino normativo; no usar JSON con binarios inline.

---

## 8. Fase F - Validación E2E

- [ ] F1. Probar request manual contra `IO.api` con la API key nueva.
- [ ] F2. Verificar `202 Accepted`.
- [ ] F3. Verificar que el `ilk` se resuelve/provisiona correctamente.
- [ ] F4. Verificar que el mensaje llega a `AI.support.rep`.
- [ ] F5. Verificar que el servicio .NET puede reproducir el mismo request.
- [x] F6. Verificar webhook outbound firmado contra receptor local de prueba.

Checks mínimos:

- `IO.api` autenticó con `tenant_id` correcto;
- no hubo `tenant_not_found`;
- no hubo `subject_data_incomplete`;
- el `dst_node` efectivo fue `AI.support.rep@<hive>`;
- el nodo AI respondió/procesó según el prompt esperado.

---

## 9. Entregables concretos de este frente

- [ ] inventario de runtimes/nodos instalados
- [ ] runtimes actualizados
- [ ] `AI.support.rep` desplegado y validado
- [ ] tenant `4iPlatform` resuelto o creado
- [ ] `IO.api` configurado con API key + integración sin webhook
- [ ] payload de referencia para .NET validado
- [ ] prueba E2E básica cerrada
- [x] webhook outbound local validado end-to-end con firma HMAC correcta

---

## 10. Observación final

Hay un punto que no conviene tratar como workaround:

- la creación del tenant `4iPlatform`

El camino correcto, según el estado actual del repo, es resolverlo por identity (`TNT_CREATE` / aprobación si aplica). No conviene simular tenancy desde el body del request ni configurar una API key con un tenant inexistente esperando que `IO.api` lo “acepte igual”, porque el adapter está diseñado para fallar cerrado.
