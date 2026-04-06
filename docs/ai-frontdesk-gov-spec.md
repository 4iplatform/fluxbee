# AI.frontdesk.gov — Especificación técnica (v1)

> ✅ Nodo **AI** (no SY), parte del “system default set”.  
> Rol: **frontdesk de government/identity** para completar identidad cuando el interlocutor todavía está en estado *temporary*.  
> Cognition: fuera de alcance (este documento describe solo comportamiento del nodo).

---

## 0) Identidad y naming

- **Nombre L2 (fijo)**: `AI.frontdesk.gov`
- **Nombre calificado**: `AI.frontdesk.gov@<hive_id>`

> Nota: `AI.frontdesk.gov` **no** reemplaza a `AI.default` (fallback general).  
> La equivalencia `frontdesk == default` queda 🧩 TBD.

---

## 1) Alcance y responsabilidades

### 1.1 Qué hace

✅ **NORMATIVO**:
- Recibe conversaciones donde `src_ilk` está presente pero la identidad asociada está **incompleta/temporary**.
- Mantiene el control conversacional hasta reunir los **datos mínimos** para completar identidad.
- Pide **confirmación explícita** al usuario antes de solicitar el “upgrade” de identidad.
- Solicita al core la **completación/upgrade** de identidad para el `src_ilk` (vía SY.admin/SY.orchestrator/identity, ver §6).
- Una vez completada la identidad:
  - informa al usuario (por IO),
  - limpia su estado por thread (LanceDB),
  - y permite que el sistema derive el caso al flujo normal (soporte/ventas/etc.) por policy.

### 1.2 Qué NO hace

✅ **NORMATIVO**:
- No escribe ni modifica identity DB directamente.
- No inventa tenants ni ILKs por su cuenta.
- No actúa como “default catch-all” del sistema (eso corresponde a `AI.default`, si existe).
- No guarda conversación completa; solo guarda **datos duros relevantes** por thread.

---

## 2) Inputs: cuándo recibe trabajo

### 2.1 Condición de entrada

✅ **NORMATIVO**:
- El mensaje entrante debe incluir:
  - `src_ilk` en `meta.src_ilk` (siempre presente; puede ser ILK temporal),
  - `thread_id` (asignado por IO; ubicación tentantiva en top-level),
  - payload `text/v1` con `content` y/o `attachments`.

Nota de transición:
- el repo ya no usa `meta.context.src_ilk` como fallback runtime; el carrier canónico es `meta.src_ilk`.

✅ **NORMATIVO**:
- `AI.frontdesk.gov` asume que la derivación a frontdesk se hace por policy/core cuando el `src_ilk` está en estado `temporary` o equivalente.

🧩 **A ESPECIFICAR (core)**:
- Señal canónica exacta de “identidad incompleta” (campo/flag) para routing a frontdesk.

### 2.2 Metadata esperada de IO

✅ **NORMATIVO (mínimo)**:
- `thread_id`: identifica conversación/canal/hilo.
- `io_context` (dentro del carrier actual): suficiente para responder por el mismo canal.

---

## 3) Estado por hilo (LanceDB)

✅ **NORMATIVO (MVP)**:
- El nodo mantiene **1 JSON** por `src_ilk` (estructura libre por prompting/policy).
- Tools mínimas (SDK/runtime):
  - `thread_state_get(state_key)`
  - `thread_state_put(state_key, data, ttl_seconds?)`
  - `thread_state_delete(state_key)`

Nota de compatibilidad:
- En runtime scoped, la clave efectiva de estado queda fijada al `src_ilk` actual.
- El alias legacy `thread_id` ya no forma parte del contrato aceptado por las tools.

### 3.1 Estructura recomendada (no normativa)

Ejemplo típico:

```json
{
  "status": "waiting_information",
  "requested_fields": ["name", "email", "phone"],
  "collected": {
    "name": null,
    "email": null,
    "phone": "+541112345678"
  },
  "tenant_hint": {
    "domain": "example.com",
    "tenant_code": null
  },
  "last_question": "¿Me confirmás tu email?"
}
```

### 3.2 Lifecycle del estado

✅ **NORMATIVO**:
- El nodo actualiza el JSON en cada turno relevante.
- El nodo **borra inmediatamente** el estado (`thread_state_delete`) cuando:
  - la identidad se completa exitosamente, o
  - el thread se cierra por policy externa (🧩).

---

## 4) Datos mínimos a recolectar

✅ **NORMATIVO (alineación core)**:
- Mínimo para completar identidad (según docs core):
  - `name`
  - `email`
  - y asociación a tenant (por dominio/código/solicitud).

🧩 **A ESPECIFICAR (core)**:
- Reglas exactas de “tenant association” (dominio, catálogo, approval, etc.).

---

## 5) Flujo conversacional (v1)

### 5.1 High-level state machine

✅ **NORMATIVO**:
1) **Detectar faltantes**: leer `thread_state` y/o extraer datos del mensaje actual.
2) **Pedir faltantes**: solicitar al usuario la información mínima que falta.
3) **Confirmación**: cuando el nodo cree tener lo mínimo, debe:
   - resumir los datos,
   - pedir confirmación explícita (“¿Confirmás que es correcto?”).
4) **Solicitar upgrade**: con confirmación positiva, disparar request al core (§6).
5) **Responder resultado**:
   - éxito: informar + limpiar estado,
   - error: informar y volver a pedir/reintentar según corresponda.

### 5.2 Reintentos

✅ **NORMATIVO**:
- Si no se obtienen mínimos tras N intentos, **por ahora no hay cutoff**: el nodo sigue pidiendo lo faltante.

---

## 6) Accion de completacion de identidad (MVP vigente)

✅ **NORMATIVO (MVP)**:
- El nodo usa la tool `ilk_register` (solo disponible en `mode=gov`).
- `ilk_register` invoca `ILK_REGISTER` via Identity (`identity_system_call_ok`).

✅ **NORMATIVO (payload mínimo esperado)**:
- `src_ilk` (el ILK a completar)
- `thread_id` (para trazabilidad)
- `identity_candidate`:
  - `name`
  - `email`
  - `phone` (si aplica)
  - `tenant_hint` (dominio/código/solicitud)

✅ **NORMATIVO (resultado)**:
- Éxito: el core devuelve identidad “complete” asociada al mismo `src_ilk` (no cambia el ILK).
- Fallo: error con razón (email ya en uso, tenant inválido, etc.).

Variables de entorno por instancia recomendadas:
- `AI_NODE_MODE=gov`
- `GOV_IDENTITY_TARGET` (default `SY.identity`)
- `GOV_IDENTITY_FALLBACK_TARGET` (opcional)
- `GOV_IDENTITY_TIMEOUT_MS` (default 10000)

---

## 7) Respuestas hacia IO (Data Plane)

✅ **NORMATIVO**:
- El nodo responde siempre por el mismo canal, usando el routing/metadata que provee IO.
- Para mensajes normales: `text/v1`.
- Para errores de contenido: `payload.type="error"` (ErrorV1Payload AI Nodes), si el consumer lo soporta; fallback texto prefijado permitido.

---

## 8) Configuración y operación (como AI Node system-default)

✅ **NORMATIVO**:
- El nodo usa el lifecycle estándar de AI Nodes:
  - config efectiva en `${STATE_DIR}/ai-nodes/AI.frontdesk.gov.json` (nombre calificado por hive a nivel sistema).
  - hot updates por Control Plane (`CONFIG_SET/GET` unicast).
  - defaults materializados en el JSON efectivo.

🧩 **A ESPECIFICAR (core)**:
- Referencia exacta en `hive.yaml`:
  - `government.identity_frontdesk` debería apuntar a `AI.frontdesk.gov@<hive_id>`.

---

## 9) Observabilidad mínima

✅ **NORMATIVO**: `STATUS` debe reportar:
- `state`: `UNCONFIGURED/CONFIGURED/FAILED_CONFIG`
- counters sugeridos:
  - `threads_active`
  - `identity_upgrades_ok`
  - `identity_upgrades_error`
- última acción relevante (timestamp)

---

## 10) Seguridad y guardrails

✅ **NORMATIVO**:
- No loggear datos sensibles en claro (email/teléfono) salvo modo debug controlado.
- No exfiltrar secretos del sistema.
- Toda acción de upgrade debe pasar por control-plane autorizado (OPA/policy).
