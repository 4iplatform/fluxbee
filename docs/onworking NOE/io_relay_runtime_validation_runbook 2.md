# IO Relay - Runbook de validacion runtime

**Fecha:** 2026-04-10
**Alcance:** validacion runtime del relay comun en `IO.slack`
**Prerequisito:** runtime/binario nuevo de `IO.slack` ya publicado o desplegable

---

## 1. Objetivo

Validar en Linux que:

- el nodo `IO.slack` toma el runtime nuevo;
- el relay comun de `io-common` queda activo;
- los logs esperados aparecen en `journalctl`;
- `config.io.relay.window_ms = 0` hace passthrough;
- `config.io.relay.window_ms > 0` permite consolidacion y flush.

---

## 2. Pregunta operativa: restart o respawn

Si ya existe un nodo `IO.slack` operativo, para actualizar codigo no hace falta respawn obligatorio.

Camino recomendado para codigo nuevo sobre nodo existente:

1. `publish` del runtime nuevo
2. `SYSTEM_UPDATE`
3. restart del nodo existente

Respawn explicito (`KILL_NODE` + `SPAWN_NODE`) conviene solo si:

- queres reinicio completamente limpio;
- el restart directo del nodo no esta disponible;
- queres recrear la instancia desde config reutilizada;
- el unit del nodo no existe o quedo inconsistente.

Resumen corto:

- nodo existente sano: `publish + update + restart`
- caso mas fuerte / limpio: `publish + update + kill + spawn`

---

## 3. Camino recomendado para nodo existente

Variables:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="IO.slack.T126@$HIVE_ID"
NEW_VERSION="0.1.1"
```

### Paso 1 - Publicar runtime nuevo

```bash
bash scripts/publish-io-runtime.sh --kind slack --version "$NEW_VERSION" --set-current --sudo
```

Guardar:

- `manifest_version`
- `manifest_hash`

### Paso 2 - Ejecutar `SYSTEM_UPDATE`

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
  -H "Content-Type: application/json" \
  -d "{
    \"category\":\"runtime\",
    \"manifest_version\": $MANIFEST_VERSION,
    \"manifest_hash\": \"$MANIFEST_HASH\"
  }"
```

### Paso 3 - Reiniciar nodo existente

Opcion automatizada recomendada:

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$NEW_VERSION" \
  --node-name "$NODE_NAME" \
  --update-existing \
  --sync-hint \
  --sudo
```

Esto hace:

- publish
- update
- restart del unit existente si lo encuentra
- solo cae a spawn si el unit no existe

Nota operativa abierta:

- ya hubo un caso donde `0.1.1` no fue tomado por el nodo existente y `0.1.0` si mostro los cambios del relay;
- si vuelve a pasar, revisar version/materializacion runtime antes de seguir con la validacion funcional.

---

## 4. Camino alternativo con respawn limpio

Usarlo solo si queres recreacion explicita de la instancia.

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$NEW_VERSION" \
  --node-name "$NODE_NAME" \
  --update-existing \
  --kill-first \
  --sync-hint \
  --sudo
```

Si necesitas reconstruir desde config actual del nodo:

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$NEW_VERSION" \
  --node-name "$NODE_NAME" \
  --reuse-existing-config \
  --spawn \
  --kill-first \
  --sync-hint \
  --sudo
```

---

## 5. Visibilidad de logs

### Runtime canonico

En el camino `publish -> update -> start.sh`, el runtime publicado deja `RUST_LOG` default incluyendo:

- `io_slack=debug`
- `io_common=debug`
- `fluxbee_sdk=info`

Por eso, en el camino canonico, los logs del relay deberian verse en `journalctl`.

### Install local por systemd

Si el nodo se levanta via `install-io.sh`, revisar:

```bash
sudo grep -n "RUST_LOG" /etc/fluxbee/io-slack.env
```

Si no esta definido, agregar:

```bash
sudo sh -c 'printf "\nRUST_LOG=info,io_slack=debug,io_common=debug,fluxbee_sdk=info\n" >> /etc/fluxbee/io-slack.env'
sudo systemctl restart fluxbee-io-slack
```

Importante:

- esto sirve para visibilidad de logs del unit local;
- no es el camino correcto para configurar el relay de un nodo spawned existente.

---

## 6. Senales esperadas al arrancar

Ver ultimos logs:

```bash
sudo journalctl -u fluxbee-node-IO.slack.T126-motherbee --since "10 min ago" --no-pager | rg -n "relay|connected to router|socket mode connected"
```

Al menos deberia aparecer:

- `io-slack relay policy initialized`

Y normalmente tambien:

- `connected to router`
- `socket mode connected`

---

## 7. Validacion A - Passthrough (`config.io.relay.window_ms = 0`)

Objetivo: confirmar que no hay espera ni consolidacion.

Config esperada en config formal del nodo:

- `config.io.relay.window_ms = 0`

Si queres forzarlo por `CONFIG_SET`:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version": 1,
    "config_version": 8,
    "apply_mode": "replace",
    "config": {
      "io": {
        "dst_node": "resolve",
        "relay": {
          "window_ms": 0,
          "max_open_sessions": 10000,
          "max_fragments_per_session": 8,
          "max_bytes_per_session": 262144
        }
      },
      "slack": {
        "app_token_ref": "env:SLACK_APP_TOKEN",
        "bot_token_ref": "env:SLACK_BOT_TOKEN"
      }
    }
  }'
```

Disparar una mencion simple al bot en Slack.

Logs a buscar:

```bash
sudo journalctl -u fluxbee-node-IO.slack.T126-motherbee --since "10 min ago" --no-pager | rg -n "relay passthrough immediate|sent to router|inbound app_mention|relay policy"
```

Resultado esperado:

- aparece `relay passthrough immediate`
- el mensaje sale al router sin hold

---

## 8. Validacion B - Relay activo (`config.io.relay.window_ms > 0`)

Objetivo: confirmar apertura, buffering y flush.

Camino recomendado:

- usar `CONFIG_SET` del nodo o spawn config formal;
- no tocar `/etc/fluxbee/io-slack.env` para esta validacion en nodos spawned.

### Paso 1 - Leer config/estado actual

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"archi"}'
```

### Paso 2 - Activar relay por `CONFIG_SET`

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version": 1,
    "config_version": 9,
    "apply_mode": "replace",
    "config": {
      "io": {
        "dst_node": "resolve",
        "relay": {
          "window_ms": 2500,
          "max_open_sessions": 10000,
          "max_fragments_per_session": 8,
          "max_bytes_per_session": 262144
        }
      },
      "slack": {
        "app_token_ref": "env:SLACK_APP_TOKEN",
        "bot_token_ref": "env:SLACK_BOT_TOKEN"
      }
    }
  }'
```

Si `app_token_ref` / `bot_token_ref` por `env:` no resuelven correctamente en ese nodo, repetir la prueba con credenciales inline:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version": 1,
    "config_version": 9,
    "apply_mode": "replace",
    "config": {
      "io": {
        "dst_node": "resolve",
        "relay": {
          "window_ms": 2500,
          "max_open_sessions": 10000,
          "max_fragments_per_session": 8,
          "max_bytes_per_session": 262144
        }
      },
      "slack": {
        "app_token": "xapp-REEMPLAZAR",
        "bot_token": "xoxb-REEMPLAZAR"
      }
    }
  }'
```

Señal típica de credenciales tomadas pero inválidas:

- `apps.connections.open failed: invalid_auth`

Si aparece eso junto con cambio de `config_generation`, el `CONFIG_SET` llegó a runtime pero Slack rechazó el token efectivo.

### Paso 3 - Confirmar respuesta del control plane

Esperado:

- `ok = true`
- `apply.hot_applied` incluye `io.relay.*` si hubo cambio efectivo
- no hace falta restart si el `CONFIG_SET` fue aceptado

### Paso 4 - Confirmar arranque/estado del relay

```bash
sudo journalctl -u fluxbee-node-IO.slack.T126-motherbee --since "10 min ago" --no-pager | rg -n "io-slack relay policy initialized|io-slack relay policy hot-applied"
```

Esperado:

- `relay_enabled=true`
- `relay_window_ms=2500`
- `relay_source=effective_config` si entro por `CONFIG_SET` y el nodo volvio a bootstrapped desde estado efectivo

### Paso 5 - Probar desde Slack

Mandar dos o mas fragmentos seguidos dentro de la ventana.

Logs a buscar:

```bash
sudo journalctl -u fluxbee-node-IO.slack.T126-motherbee --since "10 min ago" --no-pager | rg -n "relay session opened|relay fragment buffered|relay session flushed|sent to router"
```

Resultado esperado:

- `relay session opened`
- uno o mas `relay fragment buffered`
- `relay session flushed`
- despues `sent to router`

Validacion ya observada en nodo real:

- caso de un fragmento: flush por `window_elapsed` y envio al router;
- caso de dos fragmentos: `fragments=2`, `parts=2` y envio consolidado al router.

---

## 9. Validacion C - Dedup / capacidad

No es obligatoria para la primera pasada, pero sirve para comprobar comportamiento observable.

Logs a buscar:

```bash
sudo journalctl -u fluxbee-node-IO.slack.T126-motherbee --since "10 min ago" --no-pager | rg -n "relay duplicate fragment dropped|relay capacity rejected|relay max bytes reached|relay fragment dropped over byte limit"
```

Resultado esperado:

- solo aparecen si efectivamente se fuerza ese escenario;
- no deberian aparecer en una prueba nominal.

---

## 10. Confirmacion minima de exito

La validacion runtime se considera suficiente para esta fase si se cumple:

1. el nodo arranca con el runtime nuevo;
2. aparece `io-slack relay policy initialized` o `io-slack relay policy hot-applied`;
3. con `window_ms = 0` aparece `relay passthrough immediate`;
4. con `window_ms > 0` aparecen `relay session opened`, `relay fragment buffered` y `relay session flushed`;
5. el mensaje consolidado termina en `sent to router`.

Nota operativa:

- no usar `/etc/fluxbee/io-slack.env` como camino primario para validar relay en nodos spawned;
- la fuente correcta para runtime node-owned es `config.io.relay.*` por spawn config y/o `CONFIG_SET`.

---

## 11. Si algo falla

### No aparecen logs del relay

Revisar:

- `RUST_LOG` efectivo
- si el nodo realmente tomo el runtime nuevo
- si estas mirando el unit correcto en `journalctl`

### El runtime nuevo no parece cargado

Revisar:

- que `SYSTEM_UPDATE` haya quedado `ok`
- que el nodo haya sido reiniciado realmente
- si el camino usado hizo restart del unit o requiere respawn
- si reaparece el caso donde la version publicada nueva no se materializa en el nodo existente

### `CONFIG_SET` rechaza `io.relay.*`

Revisar:

- `config_version` monotonicamente mayor
- shape de `config.io.relay`
- que `max_open_sessions`, `max_fragments_per_session` y `max_bytes_per_session` sean enteros positivos
- que `window_ms` sea entero no negativo

### Duda entre restart y respawn

Regla practica:

- primero probar `publish + update + restart`
- si no toma el runtime o queres reinicio totalmente limpio, pasar a `kill + spawn`
