# AI Nodes Runbook (`ai.generic`)

## 1) Modelo

- `ai.generic` es el **único runtime AI**. Viene horneado en el `.deb`
  (`/var/lib/fluxbee/dist/runtimes/ai.generic/<versión>`) y cada instalación lo deja como `current`.
- Un nodo AI (`AI.<nombre>@<hive>`) es una **instancia** de `ai.generic`:
  - se crea con `run_node`;
  - su comportamiento entra por `CONFIG_SET`;
  - su rol, skills y handbook entran por `set_ilk_definition` sobre su ILK.
- **Ninguna instancia arranca con la instalación base.** Las instancias se crean para algo concreto;
  las de prueba son efímeras y no quedan en la base.
- `SY.frontdesk.gov` **no** es un runtime ni se despliega con este runbook. Es un nodo de sistema del
  core (`sy-frontdesk-gov.service`) y se actualiza con el `.deb`.

Variables de los ejemplos:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
```

---

## 2) Prerrequisito: la clave del proveedor

El proveedor es del hive: `ai.default_provider` en `/etc/fluxbee/hive.yaml` (`openai` si no hay
sección `ai`). La clave se carga una vez en `SY.vault` con el `resource_type` del proveedor, sin nombre
fijo:

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/vault/secrets" -H 'content-type: application/json' \
  -d '{"key":"openai_root_pool","value":{"api_key":"sk-..."},"metadata":{"tenant_id":"tnt:00000000-0000-0000-0000-000000000001","resource_type":"openai"}}'
```

- Un nodo AI busca la clave de **su tenant** y, si no hay, la del **tenant raíz**. Para que un tenant use
  su propia clave, cargala con su `tenant_id`.
- El nodo no nombra ninguna clave: un config con `behavior.vault_key` se rechaza.
- Es la misma clave que usan architect, admin, cognition y frontdesk.

---

## 3) Crear un nodo

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" -H 'content-type: application/json' \
  -d '{"node_name":"AI.sales@motherbee","runtime":"ai.generic","runtime_version":"current","tenant_id":"tnt:..."}'
```

El nodo arranca **UNCONFIGURED** hasta recibir su `CONFIG_SET`.

---

## 4) Configurarlo (`CONFIG_SET`)

1. `POST .../nodes/<node>/control/config-get` → tomar `config_version`.
2. `POST .../nodes/<node>/control/config-set` con `config_version + 1`.

Config mínima de un `ai_chat`: `behavior.kind` y `behavior.model`. Ejemplo completo:

```json
{
  "behavior": {
    "kind": "ai_chat",
    "model": "gpt-5.5",
    "instructions": {
      "source": "inline",
      "value": "Prompt del nodo...",
      "trim": true
    },
    "model_settings": {
      "temperature": 0.0,
      "top_p": 1.0,
      "max_output_tokens": 400
    }
  },
  "runtime": {
    "handler_timeout_ms": 60000,
    "worker_pool_size": 4,
    "queue_capacity": 128
  }
}
```

- El modelo tiene que ser del proveedor del hive.
- Hay **una sola** config: el `config.json` del directorio del nodo
  (`/var/lib/fluxbee/nodes/AI/<node@hive>/config.json`).
  - `CONFIG_SET` la valida, la aplica en caliente y la persiste.
  - `PUT .../config` solo la escribe; el nodo la toma en el próximo arranque.
- `CONFIG_SET` rechaza secretos (`api_key`, `api_key_env`, `secrets.*`) y los assets cognitivos
  (van por `set_ilk_definition`).

---

## 5) Actualizar `ai.generic` sin un `.deb` nuevo (desarrollo)

`scripts/deploy-ia-node.sh` publica una build del runner, manda el `SYSTEM_UPDATE` y hace spawn o
recreación. Usa `scripts/publish-ia-runtime.sh`, que compila `ai_node_runner` y lo publica con
`scripts/publish-runtime.sh`.

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "ai.generic" \
  --version "0.1.99" \
  --node-name "AI.sales@motherbee" \
  --update-existing \
  --sync-hint \
  --sudo
```

`--update-existing` hace:

- publicar el runtime nuevo;
- el update con reintentos (`targeted` por defecto);
- leer la config actual del nodo;
- `DELETE` + `POST` reusando esa config.

Para un nodo nuevo, `--spawn` con `--config-json` (y `--tenant-id`). Sin `--config-json`, el spawn usa
`config={}` y el nodo queda UNCONFIGURED.

**Scope del update:**

- `--update-scope targeted` (default): el `SYSTEM_UPDATE category=runtime` lleva `runtime` y
  `runtime_version`, así el drift de otros runtimes no bloquea el deploy.
- `--update-scope global`: solo para diagnosticar la salud general.

**Memoria inmediata (opcional):** estos flags inyectan `runtime.immediate_memory` en la config del spawn:

- `--immediate-memory-enabled <true|false>`
- `--immediate-memory-recent-max <n>`
- `--immediate-memory-active-max <n>`
- `--immediate-memory-summary-max-chars <n>`
- `--immediate-memory-refresh-every-turns <n>`
- `--immediate-memory-trim-noise <true|false>`

Sin estos flags, la config queda exactamente como viene en `--config-json`.

---

## 6) Adjuntos (estado 2026-04-06)

- `AI.*` consume `attachments[]`/`content_ref` vía el SDK AI compartido.
- Imágenes (`png`/`jpeg`/`webp`) viajan como `input_image.image_url`. Otros archivos, con
  `multimodal=true`, como `input_file.file_data` (`data:<mime>;base64,...` + `filename`).
- `file_id` / `file_url` siguen diferidos.
- **Evidencia E2E:** imagen, PDF y `xlsx`. Faltan `docx`, audio, otros binarios y mezclas.
- **Salida:** el contrato soporta `attachments[]` e `IO.slack` publica adjuntos salientes. El runner
  tiene tools que generan artefactos (csv, texto, json, markdown, html, docx, png, jpeg) y una que
  publica una página HTML con URL pública (`publish_html_page`, solo si el nodo tiene identidad).
