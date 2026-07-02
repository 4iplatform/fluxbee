# Contrato v1 — Autenticación Adapter → IO.linkedhelper usando Vault

## 1. Objetivo

Este documento define cómo debe autenticarse temporalmente la comunicación directa entre el Adapter LinkedHelper y el nodo `IO.linkedhelper` mientras no exista Edge.

La validación debe implementarse desde el momento inicial usando Vault como fuente de secretos, para no hardcodear tokens en la configuración del nodo.

---

## 2. Principio general

El `adapter_secret` pertenece al adapter y debe almacenarse en Vault.

El nodo `IO.linkedhelper` no debe recibir el secreto plano dentro de su config.

Correcto:

```json
{
  "auth": {
    "type": "vault_ref",
    "resource_type": "linkedhelper_adapter",
    "key": "linkedhelper:adapters:adp_123"
  }
}
```

Incorrecto:

```json
{
  "installation_key": "secret-plano"
}
```

---

## 3. Qué secret se guarda en Vault

Para MVP/intermedio:

```text
adapter_secret por adapter_id
```

Ejemplo de key conceptual:

```text
linkedhelper:adapters:<adapter_id>
```

Ejemplo:

```text
linkedhelper:adapters:adp_123
```

> ⚠️ **Formato de la key de Vault:** SY.vault valida `^[a-z0-9][a-z0-9:_-]*$`
> (máx 256): solo minúsculas, dígitos, `:`, `_` y `-`; sin mayúsculas ni `/`
> (rechaza con `invalid request: invalid key format`). De ahí la convención
> `linkedhelper:adapters:<adapter_id>` que publica Fluxbee Cloud. `adapter.auth.key`
> debe coincidir exactamente con esa key.

Metadata sugerida:

```json
{
  "resource_type": "linkedhelper_adapter",
  "tenant_id": "tnt_001",
  "adapter_id": "adp_123",
  "adapter_type": "linkedhelper",
  "created_by": "fluxbee_cloud",
  "status": "active"
}
```

---

## 4. Quién publica el secret

Fluxbee Cloud debe publicar el secret en Vault cuando una ManagedInstance se activa/provisiona.

Flujo:

```text
1. Adapter ya está enrolado en Fluxbee Cloud.
2. Fluxbee Cloud conoce adapter_id y adapter_secret.
3. Usuario aprueba una instancia descubierta.
4. Cloud crea ManagedInstance.
5. Cloud publica adapter_secret en Vault si aún no existe.
6. Cloud configura el nodo IO.linkedhelper con vault_ref.
```

Si el secret ya existe en Vault para ese adapter, Cloud debe reutilizarlo o verificar que coincide con el adapter activo.

---

## 5. Resolución del secret en el nodo

Para MVP se recomienda:

```text
Nodo arranca
→ lee vault_ref desde config
→ resuelve adapter_secret desde Vault
→ cachea el secret en memoria
→ valida requests contra el secret cacheado
```

El nodo no necesita consultar Vault en cada request en MVP.

Condiciones de error:

```text
Vault no disponible → nodo no operativo
Secret no encontrado → nodo no operativo
Secret inválido/vacío → nodo no operativo
vault_ref faltante → nodo no operativo
```

Estado sugerido del nodo:

```text
FAILED_CONFIG / NOT_READY / AUTH_SECRET_UNAVAILABLE
```

según la nomenclatura existente en Fluxbee.

---

## 6. Validación de requests intermedias

Mientras no exista Edge, el adapter llama directo al nodo:

```http
POST /v1/poll
X-Fluxbee-Adapter-Id: adp_123
Authorization: Bearer <adapter_secret>
Content-Type: application/json
```

El nodo debe validar:

1. Header `X-Fluxbee-Adapter-Id` presente.
2. Header `Authorization` presente.
3. Scheme `Bearer` válido.
4. `adapter_id` del header coincide con el adapter configurado.
5. Bearer secret coincide con el secret resuelto desde Vault.
6. Payload `managed_instance_id` coincide con el nodo.
7. Payload `local_instance_id` coincide con binding esperado si el nodo lo conoce.

---

## 7. Payload mínimo

```json
{
  "adapter_id": "adp_123",
  "managed_instance_id": "lhmi_001",
  "local_instance_id": "123456",
  "events": []
}
```

Reglas:

- `adapter_id` debe coincidir con `X-Fluxbee-Adapter-Id`.
- `managed_instance_id` debe coincidir con la ManagedInstance del nodo.
- `events` puede ser vacío durante MVP.
- El nodo puede responder sin acciones.

---

## 8. Response mínima

```json
{
  "ok": true,
  "actions": [],
  "accepted_at": "2026-06-29T00:00:00Z"
}
```

En MVP, `actions` queda siempre vacío.

---

## 9. Errores esperados

### Adapter no autorizado

```http
401 Unauthorized
```

Casos:

- falta Authorization;
- Bearer inválido;
- secret no coincide.

### Adapter no permitido para este nodo

```http
403 Forbidden
```

Casos:

- `adapter_id` válido pero no autorizado para este `managed_instance_id`;
- binding inconsistente.

### Payload inválido

```http
400 Bad Request
```

Casos:

- falta `managed_instance_id`;
- falta `adapter_id`;
- formato inválido.

### Nodo no operativo

```http
503 Service Unavailable
```

Casos:

- Vault no disponible;
- secret no resuelto;
- nodo sin ICH;
- config incompleta.

---

## 10. Encapsulación para futura migración al Edge

La lógica de autenticación debe quedar aislada en un módulo o servicio interno.

Ejemplo conceptual:

```text
AdapterAuthValidator
  input:
    adapter_id
    bearer_secret
    managed_instance_id
  output:
    Authorized | Unauthorized | Forbidden | ConfigError
```

Hoy:

```text
AdapterAuthValidator vive dentro de IO.linkedhelper.
```

Final:

```text
AdapterAuthValidator vive dentro del Edge.
```

El resto del procesamiento del nodo no debe depender de que la request vino directamente del adapter.

> ✅ **Estado:** implementado en `nodes/io/io-linkedhelper/src/auth.rs`
> (`AdapterAuthValidator`). Aislado del transporte HTTP (no depende de axum):
> `validate(InboundAuthRequest) -> AuthResult`, con categorías
> `BadRequest | Unauthorized | Forbidden | Unavailable` y los `error_code`
> estables de §9. `post_poll` delega toda la validación de adapter en esta capa,
> por lo que moverla al Edge es reubicar el módulo sin tocar el core del nodo.

---

## 11. Camino final con Edge

En el camino final:

```text
adapter → Edge → IO.linkedhelper
```

Responsabilidades:

```text
Edge:
- valida adapter_secret contra Vault;
- valida adapter_id;
- valida managed_instance_id;
- resuelve route/binding;
- enruta al nodo correcto.

IO.linkedhelper:
- recibe tráfico interno;
- procesa eventos de su ManagedInstance;
- no valida adapter_secret externo.
```

El nodo puede validar una credencial interna del Edge si Fluxbee define ese control.

---

## 12. Rotación y revocación

No se implementa rotación elegante en MVP.

MVP:

```text
Revocar adapter en Cloud
→ Cloud marca adapter como revoked
→ requests futuras deben fallar
→ adapter entra en needs_reenrollment
```

Si se rota el secret:

```text
Cloud actualiza Vault
→ nodo debe reiniciarse o refrescar secret
```

Para MVP, se acepta reinicio del nodo después de rotación.

Evolución posterior:

- TTL de cache;
- doble secret durante ventana de rotación;
- refresh por CONFIG_SET;
- revocación propagada por evento interno.

