# Lista de tareas — Corrección semántica de `ICH` en Fluxbee

**Estado:** backlog inicial para implementación  
**Fecha:** 2026-05-20  
**Base:** `docs/onworking NOE/especificacion_correccion_ich_fluxbee.md`

---

## 1. Objetivo

Ordenar el trabajo necesario para corregir la semántica de `ICH` en:

- documentación core;
- SDK/Identity;
- nodos IO;
- y tests de no regresión.

El principio rector es:

```text
ICH = canal local/sistémico operado por una instancia de nodo IO
```

y no:

```text
ICH = handle remoto del interlocutor externo
```

---

## 2. Orden sugerido de trabajo

El orden recomendado es:

1. Documentación core y vocabulario canónico
2. Relevamiento documental de SDK / Identity API y nombres ambiguos
3. Lifecycle de nodos IO respecto a ICH
4. Producción de mensajes IO (`meta.ich`, `thread_id`)
5. Dispatch/respuesta por canal local
6. Tests unitarios y E2E

Este orden busca fijar primero la semántica y recién después bajar cambios de implementación.

---

## 3. Bloque A — documentación core

- [x] Corregir `docs/10-identity-v2.md` para dejar explícito que `ICH` pertenece al sistema Fluxbee y no al interlocutor externo
- [x] Corregir `docs/02-protocolo.md` para redefinir `meta.ich` como canal local/sistémico
- [x] Corregir `docs/04-routing.md` para aclarar el rol de `meta.ich` respecto de `thread_id/thread_seq`
- [x] Agregar warning fuerte en `docs/11-context.md` indicando que es histórico/legacy y que sus ejemplos de `ICH` no son canónicos hoy
- [x] Revisar `docs/12-cognition-v2.md` para que los ejemplos de `thread_id` no sugieran un `ICH` externo
- [x] Revisar specs IO activas para separar de forma explícita:
  - `ICH local`
  - `remote handle`
  - `ILK externo`

### Resultado esperado

La documentación activa no debe permitir la lectura:

```text
ICH = canal del usuario externo
```

---

## 4. Bloque B — SDK / Identity (solo relevamiento y documentación)

**Restricción actual:** este bloque no se implementa en core/SDK por ahora.  
Si se detectan gaps, deben dejarse documentados, pero no se toca código de core salvo documentación ajustada al estado real actual.

- [ ] Revisar `crates/fluxbee_sdk/src/identity.rs` y documentar ambigüedades actuales del campo `address`
- [ ] Identificar APIs del SDK que hoy puedan inducir a resolver identidad externa desde `ICH`
- [ ] Revisar `src/bin/sy_identity.rs` y documentar dónde el ownership de `ICH` todavía no queda suficientemente explícito
- [ ] Documentar constraints de duplicidad actuales de `ICH` y qué habría que revisar más adelante
- [ ] Documentar gaps de eventos/auditoría de Identity respecto del ownership de `ICH`

### Resultado esperado

Identity y SDK no deberían incentivar ni normalizar flujos tipo:

```text
persona externa -> ICH propio
```

Pero en esta etapa el objetivo es dejar ese gap claramente relevado, no corregirlo en código.

---

## 5. Bloque C — lifecycle de nodos IO

- [x] Definir contrato operativo mínimo:
  - nodo vivo
  - control-plane disponible
  - no operativo para tráfico real sin `ICH`
- [ ] Revisar cómo cada nodo IO resuelve su ILK interno al boot
- [ ] Revisar cómo cada nodo IO obtiene el material local para resolver/registrar su `ICH`
- [ ] Implementar/ajustar chequeo de no-operatividad si falta `ICH`
- [ ] Asegurar que el estado del nodo sea claro al estar vivo pero no operativo

### Casos mínimos a revisar

- [x] `IO.api`
- [x] `IO.slack`
- [ ] `IO.linkedhelper`
- [ ] otros nodos IO activos según inventario real

### Nota para `IO.api`

Como criterio inicial, puede tomarse el puerto/entrypoint local como base del asset/`ICH`, y luego refinar el material exacto de unicidad.

### Hallazgos actuales — `IO.api`

- [x] Lee `self_ilk_id` / `self_tenant_id` al boot
- [x] Usa `listen.address` como `entrypoint` operativo en `IoContext`
- [x] Calcula `thread_id` con `PersistentChannel(channel_type=\"api\", entrypoint_id=listen.address, conversation_id=...)`
- [ ] No tiene todavía una resolución/registro explícito y homogéneo de `ICH` propio de la instancia

Lectura actual:
- `thread_id` en `IO.api` ya usa material local suficiente para distinguir el canal operativo;
- el gap pendiente está en la explicitación y lifecycle del `ICH` de la propia instancia, no en el cálculo actual de `thread_id`.
- el camino correcto para cerrarlo depende de core: asociación formal del canal propio vía `ILK_ADD_CHANNEL` para nodos IO generales.
- decisión actual: **no** implementar workaround de `ICH` sintético local.

### Hallazgos actuales — `IO.slack`

- [x] Lee `self_ilk_id` / `self_tenant_id` al boot
- [x] Usa `self_ilk_id` / `self_tenant_id` operacionalmente para resolver credenciales `slack` en vault y refrescarlas
- [x] Usa `team_id` / workspace como `entrypoint` operativo en `IoContext`
- [x] Calcula `thread_id` con material local del canal:
  - `NativeThread(... entrypoint_id=team_id ...)` cuando existe `thread_ts`
  - `PersistentChannel(... entrypoint_id=team_id ...)` cuando no existe `thread_ts`
- [ ] No tiene todavía una resolución/registro explícito y homogéneo de `ICH` propio de la instancia/canal Slack

Lectura actual:
- `IO.slack` ya usa tanto ILK interno como material local del canal de forma operativa real;
- el gap pendiente está en la explicitación/registro homogéneo del `ICH` local, no en el cálculo actual de `thread_id` ni en el uso de `team_id`.
- el camino correcto para cerrarlo depende de core: asociación formal del canal propio vía `ILK_ADD_CHANNEL` para nodos IO generales.
- decisión actual: **no** implementar workaround de `ICH` sintético local.

### Estado actual del bloque

- El contrato mínimo ya quedó documentado en las specs.
- La implementación homogénea por nodo sigue pendiente.
- En el estado actual del repo, la no-operatividad suele expresarse por `UNCONFIGURED` / `FAILED_CONFIG`, no por un estado separado específico de `ICH`.

---

## 6. Bloque D — producción de mensajes IO

- [ ] Revisar en cada nodo IO cómo se completa `meta.ich`
- [ ] Corregir paths donde `meta.ich` hoy represente o derive del interlocutor externo
- [ ] Confirmar que el `src_ilk` externo se resuelve por mecanismos de identity/contact/alias, no por `ICH`
- [ ] Revisar cálculo de `thread_id` en cada nodo IO o en el SDK que ese nodo usa
- [ ] Verificar que el cálculo actual de `thread_id` siga como está donde ya sea correcto
- [ ] Detectar nodos donde `thread_id` siga ambiguo o no incorpore el `ICH` local cuando corresponde

### Criterio acordado

- `thread_id` se calcula
- el cálculo queda en el nodo IO o SDK que conoce el medio
- si un nodo no lo tiene bien resuelto hoy, se revisa el caso puntual

---

## 7. Bloque E — dispatch y salida

- [ ] Revisar cómo cada nodo IO usa `meta.ich` para decidir el canal local de salida
- [ ] Ajustar dispatch para que `ICH` represente siempre el asset local correcto
- [ ] Revisar que la selección del destinatario remoto use identidad/contact handle externo y no `ICH`

### Resultado esperado

El sistema debe poder distinguir correctamente:

- mismo interlocutor externo
- distintos assets locales
- distinto canal de salida
- distinto `thread_id` cuando corresponda

---

## 8. Bloque F — tests

- [ ] Test unitario: mismo remoto + mismo `ICH` local => mismo `thread_id`
- [ ] Test unitario: mismo remoto + distinto `ICH` local => distinto `thread_id`
- [ ] Test unitario: mismo `ICH` local + distinto thread nativo => distinto `thread_id`
- [ ] Test/E2E: nodo IO sin `ICH` queda vivo pero no operativo
- [ ] Test/E2E: nodo IO con config suficiente resuelve/obtiene `ICH`
- [ ] Test/E2E: mensaje entrante usa `meta.ich` local
- [ ] Test/E2E: no se crea `ICH` para interlocutor externo
- [ ] Test/E2E: dispatch de salida usa el `ICH` local correcto
- [ ] Test/E2E: no se agregan nuevos usos de `ctx*` en paths nuevos

---

## 9. Priorización práctica sugerida

Si hay que elegir un camino incremental:

### Fase 1

- [x] Corregir documentación core principal
- [ ] Aclarar en documentación/diagnóstico dónde SDK/Identity mantiene ambigüedad en `address`
- [ ] Definir contrato operativo “vivo pero no operativo sin ICH”

### Fase 2

- [ ] Revisar `meta.ich` y `thread_id` en nodos IO actuales
- [ ] Ajustar `IO.api` como caso inicial de asset/ICH local
- [ ] Ajustar dispatch de salida

### Fase 3

- [ ] Completar tests unitarios y E2E
- [ ] Revisar docs históricas/legacy y mover o marcar lo que siga induciendo error

---

## 10. Criterio de cierre

El trabajo puede darse por bien encaminado cuando:

- la documentación activa ya no sugiera que `ICH` es externo;
- los nodos IO operen con `ICH` local;
- `thread_id` use material correcto por nodo/SDK;
- los nodos puedan permanecer vivos pero no operativos sin `ICH`;
- y existan tests que cubran la separación entre:
  - `ICH local`
  - `remote handle`
  - `ILK externo`
  - `thread_id`
  - `dispatch de salida`
