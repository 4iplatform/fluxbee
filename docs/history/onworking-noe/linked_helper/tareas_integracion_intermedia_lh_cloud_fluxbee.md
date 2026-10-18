# Tareas técnicas — Integración intermedia Fluxbee Cloud ↔ Fluxbee ↔ Adapter LinkedHelper

## 1. Objetivo

Cerrar el flujo end-to-end local sin Edge:

```text
Fluxbee Cloud aprueba instancia
→ provisiona nodo IO.linkedhelper
→ devuelve report_to directo al adapter
→ adapter reporta al nodo
→ nodo valida y acepta eventos
```

Este trabajo es intermedio. Debe dejar preparado el reemplazo futuro por Edge.

---

## 2. Tareas en Fluxbee Cloud

### 2.1 Modelo de provisioning de nodo

Agregar o completar persistencia para ManagedInstance:

```text
managed_instance_id
node_name
hive_id
runtime_name
runtime_version
provisioning_status
report_to_kind
report_to_url
report_to_route_id nullable
node_port nullable
last_provisioning_error nullable
```

Valores intermedios:

```text
runtime_name = io.linkedhelper
node_name = IO.linkedhelper.<managed_instance_id>@motherbee
report_to_kind = direct_node_http
```

---

### 2.2 Acción de provisionamiento

Agregar acción en Cloud:

```text
Provision IO.linkedhelper node
```

Disparadores posibles:

- al aprobar una discovered instance;
- botón manual desde UI;
- retry si provisioning falló.

La acción debe:

1. Validar que la ManagedInstance existe.
2. Validar que tiene adapter binding.
3. Guardar/publicar adapter_secret en Vault.
4. Construir config del nodo.
5. Llamar a Fluxbee Admin API para `run_node`.
6. Verificar que el nodo quedó arriba.
7. Guardar `report_to`.
8. Cambiar estado a `node_active` / `reporting_enabled`.

---

### 2.3 Publicación del secret en Vault

Cloud debe publicar el `adapter_secret` en Vault con una key estable:

```text
linkedhelper:adapters:<adapter_id>
```

Metadata:

```json
{
  "resource_type": "linkedhelper_adapter",
  "tenant_id": "...",
  "adapter_id": "...",
  "adapter_type": "linkedhelper"
}
```

El nodo recibe solo `vault_ref`.

---

### 2.4 Construcción de config del nodo

Config conceptual:

```json
{
  "managed_instance_id": "lhmi_001",
  "tenant_id": "tnt_001",
  "adapter": {
    "adapter_id": "adp_123",
    "local_instance_id": "123456",
    "auth": {
      "type": "vault_ref",
      "resource_type": "linkedhelper_adapter",
      "key": "linkedhelper:adapters:adp_123"
    }
  },
  "listen": {
    "address": "0.0.0.0",
    "port": 19091
  },
  "mode": "direct_http_intermediate"
}
```

---

### 2.5 Devolver `report_to` en desiredState

En `alive`, Cloud debe devolver bindings activos con `reportTo`:

```json
{
  "desiredState": {
    "bindings": [
      {
        "managedInstanceId": "lhmi_001",
        "localInstanceId": "123456",
        "status": "active",
        "reportTo": {
          "kind": "direct_node_http",
          "url": "http://localhost:19091/v1/poll",
          "auth": "adapter_secret"
        }
      }
    ]
  }
}
```

---

### 2.6 UI mínima

Mostrar por ManagedInstance:

- estado de provisioning;
- node_name;
- runtime;
- report_to.kind;
- report_to.url;
- último error;
- botón retry provision;
- botón disable/reporting off.

---

## 3. Tareas en Fluxbee runtime

### 3.1 Publicar runtime `io.linkedhelper` — HECHO

Agregar runtime al camino estándar de publicación.

Definiciones:

```text
runtime_name = io.linkedhelper
binary/crate = nodes/io/io-linkedhelper
```

Seguir patrón de `io.api` / `io.slack` si existe.

**Estado (wiring en repo):** cableado siguiendo el patrón de `io.api`/`io.slack`.
- `scripts/install.sh` compila `-p io-linkedhelper` y publica el runtime
  `io.linkedhelper@1.0.0` en `$STATE_DIR/dist/runtimes` vía
  `scripts/publish-io-linkedhelper-runtime.sh` (con verificación de binario).
- `lab/Dockerfile` (fat) y `lab/Dockerfile.slim` (builder) compilan
  `-p io-linkedhelper`, por lo que el binario y el runtime publicado viajan a
  ambas imágenes del lab.
- `scripts/deploy-io-linkedhelper.sh` es el rollout turnkey (publish →
  `POST /hives/{id}/update` → spawn/restart), espejo de `deploy-io-slack.sh`.

---

### 3.2 Hacer nodo 1:1 con ManagedInstance

Actualizar `IO.linkedhelper` para operar como:

```text
1 nodo = 1 managed_instance_id = 1 cuenta LH aprobada
```

Eliminar o aislar la lógica anterior donde un nodo podía representar varios perfiles/adapters internamente.

---

### 3.3 Config del nodo

El nodo debe requerir:

```text
managed_instance_id
adapter.adapter_id
adapter.auth.vault_ref
listen.address
listen.port
```

Opcional:

```text
tenant_id
adapter.local_instance_id
mode
```

Si falta config obligatoria, no debe quedar operativo.

---

### 3.4 Listen desde config

El nodo debe poder escuchar según config:

```json
{
  "listen": {
    "address": "0.0.0.0",
    "port": 19091
  }
}
```

Si hoy depende de `LISTEN_ADDR`, definir una de estas opciones:

- migrar a `config.listen`;
- mapear `config.listen` a env al levantar;
- soportar ambos, con precedencia documentada.

Recomendación: usar `config.listen` para que Fluxbee Cloud pueda provisionar nodos de forma declarativa.

---

### 3.5 Resolver secret desde Vault

Al arrancar:

```text
leer vault_ref
resolver adapter_secret desde Vault
cachear en memoria
si falla, nodo no operativo
```

No usar secreto plano en config.

---

### 3.6 Endpoint HTTP intermedio

Exponer:

```http
POST /v1/poll
```

Validar:

```text
X-Fluxbee-Adapter-Id
Authorization: Bearer <adapter_secret>
managed_instance_id
local_instance_id si aplica
```

Responder:

```json
{
  "ok": true,
  "actions": []
}
```

En esta etapa no se devuelven acciones reales.

---

### 3.7 Encapsular auth — HECHO

Crear capa interna aislada:

```text
AdapterAuthValidator
```

Debe poder moverse luego al Edge.

**Estado:** implementado en `nodes/io/io-linkedhelper/src/auth.rs` como
`AdapterAuthValidator`. Es transport-agnóstico (recibe `Option<&str>`, devuelve
un `AuthResult` con categoría `BadRequest|Unauthorized|Forbidden|Unavailable` +
`error_code`/`error_message`); el handler HTTP solo mapea la categoría a un
status. `post_poll` ya no valida inline: construye el validator desde el binding
y delega. Cubierto por tests unitarios (una por cada rechazo + happy path). Para
migrar al Edge solo hay que mover el módulo y alimentarlo con los mismos inputs.

---

### 3.8 ICH propio — HECHO

El nodo debe asegurar ICH propio:

```text
ICH.linkedhelper.<managed_instance_id>
```

Si no puede registrar/resolver ICH propio, no debe quedar operativo.

**Estado:** el nodo registra su ICH propio vía
`io_common::provision::ensure_own_ich` (canal `linkedhelper_managed_instance`,
address = `managed_instance_id`) en el boot (cuando hay config efectiva) y en
cada `CONFIG_SET`, siguiendo el patrón de `io.slack`. Si falla, el nodo queda en
`FAILED_CONFIG` con `own_ich_registration_failed` y no pasa a `Configured`. El
`ich_id` resultante se expone en `STATUS` / `CONFIG_GET` (`runtime.own_ich_id`).

---

### 3.9 Idempotencia de eventos — HECHO

El endpoint `/v1/poll` es at-least-once (el adapter reintenta). El nodo mantiene
un ledger durable (`processed_events` en el state) que registra la respuesta
**terminal** por evento (`managed_instance_id:adapter_id:event_id`). Un evento
reenviado se responde con la misma respuesta registrada (replay) sin repetir
efectos (doble dispatch al router, doble provision). Los fallos **retryable** no
se registran, para que el adapter pueda reintentarlos. El ledger es acotado
(`http.dedup_max_entries`, default 10000) con evicción del más antiguo.

---

## 4. Tareas en Docker/lab local

### 4.1 Exponer puerto del nodo

Agregar puerto para pruebas locales:

```yaml
ports:
  - "19091:19091"
```

Para múltiples nodos:

```yaml
ports:
  - "19091:19091"
  - "19092:19092"
  - "19093:19093"
```

---

### 4.2 Bind correcto

El nodo debe escuchar en:

```text
0.0.0.0:<port>
```

No en:

```text
127.0.0.1:<port>
```

---

### 4.3 Documentar limitación

La publicación de puertos por nodo es solo para el camino intermedio sin Edge.

En el diseño final, el adapter reporta a Edge y no necesita conocer puertos por nodo.

---

## 5. Tareas en Adapter LinkedHelper

### 5.1 Consumir `report_to` desde desiredState

El adapter debe persistir por binding:

```text
managed_instance_id
local_instance_id
report_to.kind
report_to.url
report_to.route_id nullable
```

---

### 5.2 Reporte directo intermedio

Si `report_to.kind = direct_node_http`, el adapter debe enviar:

```http
POST <report_to.url>
X-Fluxbee-Adapter-Id: <adapter_id>
Authorization: Bearer <adapter_secret>
```

Payload:

```json
{
  "adapter_id": "adp_123",
  "managed_instance_id": "lhmi_001",
  "local_instance_id": "123456",
  "events": []
}
```

---

### 5.3 No implementar acciones todavía

Por ahora el adapter debe aceptar response con:

```json
{
  "ok": true,
  "actions": []
}
```

Si `actions` trae datos no soportados, loguear warning e ignorar.

---

### 5.4 Preparar futuro Edge

El adapter debe tratar `report_to.kind` como enum.

Valores esperados:

```text
direct_node_http
edge_route
```

Si recibe `edge_route` en el futuro, deberá reportar al Edge, no al nodo directo.

---

## 6. Orden recomendado de implementación

1. Fluxbee runtime: publicar runtime `io.linkedhelper`.
2. Fluxbee runtime: adaptar nodo a `managed_instance_id` único.
3. Fluxbee runtime: soportar `config.listen` y Vault ref.
4. Docker/lab: exponer puerto del nodo.
5. Cloud: publicar secret en Vault.
6. Cloud: provisionar nodo y guardar report_to.
7. Cloud: devolver report_to en desiredState.
8. Adapter: consumir report_to.
9. Adapter: reportar directo al nodo.
10. Prueba E2E local.

---

## 7. Criterios de aceptación

- Cloud puede aprobar una instancia y crear ManagedInstance.
- Cloud puede publicar secret en Vault.
- Cloud puede levantar nodo `IO.linkedhelper.<managed_instance_id>@motherbee`.
- Nodo resuelve secret desde Vault.
- Nodo escucha en puerto publicado.
- Cloud devuelve `report_to` al adapter.
- Adapter reporta directo al nodo.
- Nodo valida request.
- Nodo responde `ok: true` con `actions: []`.

---

## 8. Deuda explícita posterior

- Reemplazar `direct_node_http` por `edge_route`.
- Mover validación de adapter del nodo al Edge.
- Implementar sync batch real.
- Implementar acciones de vuelta al adapter.
- Resolver AI/workflow routing definitivo.
- Automatizar asignación de puertos solo para lab o eliminarla con Edge.
- Completar rotación/revocación de secrets.
- Implementar updates del adapter.

