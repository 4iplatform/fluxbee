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

Para esta integración no hace falta webhook.

Entonces la integración en `IO.api` debe quedar con:

- `final_reply_required = false`
- sin callback final obligatorio

Igual sigue siendo recomendable modelar una `integration_id`, porque el contrato actual de `IO.api` ya separa:

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

Como esta integración no necesita webhook:

- `final_reply_required = false`
- `integration_id` igualmente presente

Recomendado:

- `integration_id = "int_4iplatform_whatsapp"`
- `key_id = "4iplatform-main"`

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
        "tenant_id": "tnt:XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX",
        "integration_id": "int_4iplatform_whatsapp",
        "token": "fb_api_4iplatform_001"
      }
    ]
  },
  "integrations": [
    {
      "integration_id": "int_4iplatform_whatsapp",
      "tenant_id": "tnt:XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX",
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

---

## 7. Fase E - Contrato HTTP para el servicio .NET

### E.1 Reglas

El request debe ir a:

```http
POST /
Authorization: Bearer <API_KEY>
Content-Type: application/json
```

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

---

## 8. Fase F - Validación E2E

- [ ] F1. Probar request manual contra `IO.api` con la API key nueva.
- [ ] F2. Verificar `202 Accepted`.
- [ ] F3. Verificar que el `ilk` se resuelve/provisiona correctamente.
- [ ] F4. Verificar que el mensaje llega a `AI.support.rep`.
- [ ] F5. Verificar que el servicio .NET puede reproducir el mismo request.

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

---

## 10. Observación final

Hay un punto que no conviene tratar como workaround:

- la creación del tenant `4iPlatform`

El camino correcto, según el estado actual del repo, es resolverlo por identity (`TNT_CREATE` / aprobación si aplica). No conviene simular tenancy desde el body del request ni configurar una API key con un tenant inexistente esperando que `IO.api` lo “acepte igual”, porque el adapter está diseñado para fallar cerrado.
