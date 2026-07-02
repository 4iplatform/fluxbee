# Especificación intermedia v1 — Fluxbee Cloud, Adapter LinkedHelper y nodo IO.linkedhelper sin Edge

## 1. Objetivo

Este documento define el camino intermedio para cerrar un flujo end-to-end local entre:

- Fluxbee Cloud;
- Adapter LinkedHelper;
- Fluxbee runtime;
- nodo `IO.linkedhelper`.

El objetivo inmediato es poder probar que:

1. Fluxbee Cloud aprueba una instancia LinkedHelper descubierta.
2. Fluxbee Cloud levanta o registra un nodo `IO.linkedhelper` asociado a esa instancia.
3. Fluxbee Cloud confirma que el nodo quedó disponible.
4. Fluxbee Cloud informa al adapter dónde reportar esa instancia.
5. El adapter empieza a reportar directamente al nodo.

Este flujo es **intermedio** porque todavía no existe el Edge/ingress definitivo. Por lo tanto, el adapter reportará directamente al nodo `IO.linkedhelper` por HTTP.

El diseño final será distinto: el adapter reportará al Edge, el Edge validará/ruteará, y el nodo recibirá tráfico interno.

---

## 2. Principio de diseño

La arquitectura definitiva debe ser:

```text
Adapter LinkedHelper
→ Edge LinkedHelper / RT Edge
→ IO.linkedhelper.<managed_instance_id>@motherbee
```

Pero para cerrar pruebas locales sin Edge, se permite temporalmente:

```text
Adapter LinkedHelper
→ IO.linkedhelper.<managed_instance_id>@motherbee directo por HTTP
```

Esta ruta directa debe quedar encapsulada como `report_to.kind = direct_node_http`, para poder reemplazarla luego por `report_to.kind = edge_route` sin cambiar el resto del modelo conceptual.

---

## 3. Identidades canónicas

### 3.1 ManagedInstance

Una instancia LinkedHelper aprobada en Fluxbee Cloud debe tener una identidad canónica:

```text
managed_instance_id
```

Ejemplo:

```text
lhmi_001
```

`managed_instance_id` es la identidad Fluxbee de la cuenta/instancia LinkedHelper administrada.

No debe usarse como identidad canónica:

- `local_instance_id` solo;
- carpeta de 6 dígitos de LinkedHelper;
- `adapter_id + local_instance_id` como identidad final del nodo;
- datos de contacto externo.

`adapter_id + local_instance_id` representa solo el binding técnico actual entre un adapter instalado y una instancia local detectada.

---

### 3.2 Nodo IO.linkedhelper

Nombre canónico del nodo:

```text
IO.linkedhelper.<managed_instance_id>@motherbee
```

Ejemplo:

```text
IO.linkedhelper.lhmi_001@motherbee
```

Regla:

```text
1 ManagedInstance activa = 1 nodo IO.linkedhelper lógico
```

Si un mismo adapter tiene tres cuentas aprobadas, puede terminar reportando a tres nodos distintos.

---

### 3.3 ICH

Nombre conceptual del ICH:

```text
ICH.linkedhelper.<managed_instance_id>
```

Ejemplo:

```text
ICH.linkedhelper.lhmi_001
```

El ICH pertenece al nodo IO interno. No representa un contacto externo ni una conversación LinkedIn.

---

## 4. Flujo final esperado con Edge

El flujo definitivo debe ser:

```text
1. Adapter se enrola en Fluxbee Cloud.
2. Adapter reporta discovery de instancias locales.
3. Usuario aprueba una instancia en Fluxbee Cloud.
4. Fluxbee Cloud crea ManagedInstance.
5. Fluxbee Cloud provisiona nodo IO.linkedhelper para esa ManagedInstance.
6. Fluxbee Cloud configura binding ManagedInstance → nodo.
7. Fluxbee Cloud devuelve al adapter un endpoint Edge + route_id.
8. Adapter envía batches agrupados por managed_instance_id al Edge.
9. Edge autentica adapter_secret.
10. Edge separa eventos por managed_instance_id.
11. Edge enruta cada grupo al nodo IO.linkedhelper correcto.
12. Nodo procesa eventos internos.
13. Acciones pendientes vuelven al adapter en la response del Edge.
```

En el camino final, el adapter no conoce puertos ni URLs internas de nodos.

---

## 5. Flujo intermedio local sin Edge

Mientras no exista Edge, el flujo será:

```text
1. Adapter se enrola en Fluxbee Cloud.
2. Adapter reporta discovery.
3. Usuario aprueba una instancia en Cloud.
4. Cloud crea ManagedInstance.
5. Cloud guarda el secret del adapter en Vault.
6. Cloud provisiona nodo IO.linkedhelper asociado a esa ManagedInstance.
7. Cloud verifica que el nodo levantó.
8. Cloud guarda report_to directo al nodo.
9. Cloud devuelve report_to al adapter vía alive / desiredState.
10. Adapter reporta directo al nodo.
11. Nodo valida adapter_secret resolviendo desde Vault/caché local.
12. Nodo acepta eventos.
13. Nodo no devuelve acciones todavía, salvo respuesta vacía/ack.
```

---

## 6. `report_to` intermedio

En el flujo intermedio, Fluxbee Cloud debe devolver al adapter algo similar a:

```json
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
```

`kind = direct_node_http` significa:

- el adapter reporta directo al nodo;
- el nodo expone HTTP externo temporalmente;
- esta ruta existe solo porque todavía no hay Edge.

El diseño final debería devolver:

```json
{
  "reportTo": {
    "kind": "edge_route",
    "url": "https://edge.fluxbee.com/linkedhelper/sync",
    "routeId": "route_lhmi_001",
    "auth": "adapter_secret"
  }
}
```

---

## 7. Configuración del nodo IO.linkedhelper intermedio

Cada nodo `IO.linkedhelper` debe configurarse para una sola ManagedInstance.

Configuración conceptual:

```json
{
  "managed_instance_id": "lhmi_001",
  "tenant_id": "tnt_001",
  "adapter": {
    "adapter_id": "adp_123",
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

Notas:

- El nodo no debe recibir el `adapter_secret` plano dentro de su config.
- El nodo recibe una referencia a Vault.
- El nodo debe quedar no operativo si no puede resolver su secret desde Vault.
- El nodo debe quedar no operativo si no tiene `managed_instance_id`.
- El nodo debe quedar no operativo si no tiene ICH propio asociado.

---

## 8. Autenticación intermedia

Mientras no exista Edge, el adapter llamará al nodo directo:

```http
POST /v1/poll
X-Fluxbee-Adapter-Id: adp_123
Authorization: Bearer <adapter_secret>
Content-Type: application/json
```

El nodo debe validar:

1. `X-Fluxbee-Adapter-Id` coincide con el adapter configurado/autorizado.
2. `Authorization: Bearer` coincide con el secreto resuelto desde Vault.
3. El payload corresponde al `managed_instance_id` del nodo.
4. El `local_instance_id`, si viene, corresponde al binding conocido.

Payload mínimo intermedio:

```json
{
  "adapter_id": "adp_123",
  "managed_instance_id": "lhmi_001",
  "local_instance_id": "123456",
  "events": []
}
```

Aunque el nodo ya represente `lhmi_001`, el payload debe incluir `managed_instance_id` para validación defensiva y futura compatibilidad con Edge.

---

## 9. Encapsulación para mover validación al Edge

La validación de adapter debe implementarse en una capa aislada.

Ejemplo conceptual:

```text
AdapterAuthValidator
  validate(adapter_id, bearer_secret, managed_instance_id)
```

Hoy esa capa vive dentro de `IO.linkedhelper`.

En el diseño final, esa misma responsabilidad debe moverse al Edge:

```text
AdapterAuthValidator
  ubicación intermedia: IO.linkedhelper
  ubicación final: Edge LinkedHelper
```

El nodo debe quedar preparado para que en el futuro reciba tráfico interno ya validado por Edge.

> ✅ **Estado:** `AdapterAuthValidator` ya existe como módulo aislado
> (`io-linkedhelper/src/auth.rs`), transport-agnóstico y con tests. Además el
> nodo ya registra su ICH propio (`ensure_own_ich`, §3.3/§7) en boot + CONFIG_SET
> y queda en `FAILED_CONFIG` (`own_ich_registration_failed`) si no puede, y el
> endpoint `/v1/poll` es idempotente por `event_id` (ledger durable de respuestas
> terminales). Pendiente real: tráfico interno validado por Edge.

---

## 10. Docker local

Para que el adapter corra en la máquina host y el nodo corra dentro del contenedor Docker, el puerto del nodo debe publicarse.

Ejemplo:

```yaml
ports:
  - "19091:19091"
```

El nodo debe escuchar en:

```text
0.0.0.0:19091
```

No debe escuchar en:

```text
127.0.0.1:19091
```

porque `127.0.0.1` dentro del contenedor no es accesible desde el host.

Para múltiples nodos locales sin Edge, se requiere un puerto distinto por nodo:

```text
lhmi_001 → 19091
lhmi_002 → 19092
lhmi_003 → 19093
```

Esto es una limitación del camino intermedio. En el camino final, el adapter reportará al Edge y no necesitará conocer puertos por nodo.

---

## 11. Publicación de runtime `io.linkedhelper`

El runtime `io.linkedhelper` debe publicarse por el camino estándar de Fluxbee, siguiendo el patrón de otros nodos IO.

Definiciones:

```text
runtime_name = io.linkedhelper
node_name = IO.linkedhelper.<managed_instance_id>@motherbee
binary/crate = nodes/io/io-linkedhelper
```

Si actualmente no existe publish/deploy para este runtime, debe agregarse.

**Estado:** ya agregado. `scripts/install.sh` compila y publica `io.linkedhelper`
(vía `scripts/publish-io-linkedhelper-runtime.sh`), `lab/Dockerfile` y
`lab/Dockerfile.slim` lo compilan, y `scripts/deploy-io-linkedhelper.sh` cubre el
rollout remoto (publish + `update` + spawn) siguiendo el patrón de `io.api`/`io.slack`.

---

## 12. Responsabilidad de Fluxbee Cloud

Fluxbee Cloud debe:

1. Crear ManagedInstance al aprobar una instancia descubierta.
2. Guardar el adapter secret en Vault.
3. Provisionar el nodo `IO.linkedhelper.<managed_instance_id>@motherbee`.
4. Verificar que el nodo levantó.
5. Persistir `node_name`, `runtime`, `hive`, `report_to`, estado y errores.
6. Devolver `report_to` al adapter vía `alive` / `desiredState`.
7. Mantener visible en UI el estado de provisioning.

Estados sugeridos:

```text
discovered
approved
provisioning_node
node_active
reporting_enabled
error
 disabled
```

---

## 13. Qué queda fuera del camino intermedio

No se implementa todavía:

- Edge real;
- sync batch completo;
- acciones de vuelta al adapter;
- AI routing definitivo;
- update automático;
- config segura definitiva;
- múltiples acciones pendientes por response;
- rollback de nodos;
- route_id real.

---

## 14. Criterios de aceptación del camino intermedio

Se considera exitoso si:

1. Una instancia descubierta se aprueba en Fluxbee Cloud.
2. Fluxbee Cloud crea una ManagedInstance.
3. Fluxbee Cloud guarda el secret del adapter en Vault.
4. Fluxbee Cloud levanta o registra un nodo `IO.linkedhelper.<managed_instance_id>@motherbee`.
5. El nodo resuelve el secret desde Vault.
6. El nodo expone HTTP en un puerto accesible desde el host.
7. Fluxbee Cloud devuelve `report_to.kind = direct_node_http` al adapter.
8. El adapter reporta al nodo usando `adapter_secret`.
9. El nodo valida la request.
10. El nodo responde ack vacío o sin acciones.

