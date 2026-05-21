# Lista de tareas — `IO.api` hacia `ICH` estable por integración

**Estado:** backlog inicial para implementación  
**Fecha:** 2026-05-21  
**Base:** `docs/onworking NOE/especificacion_ich_multiinstancia_io_slack_api.md`

---

## 1. Lectura de la decisión

Para `IO.api`, la parte correcta y aplicable de la especificación es:

- el `ICH` no debe derivarse del interlocutor externo;
- el `ICH` no debe usar `listen_addr` como identidad canónica;
- el `ICH` debe representar una integración/canal API estable;
- `listen_addr` queda como configuración de runtime, no como identidad.

Importante:

- los nombres tipo `IO.api.<integration_id>@motherbee` deben tratarse como **modelo objetivo**;
- no son precondición para empezar a corregir el nodo;
- el paso inmediato viable es hacer que el `ICH` propio se derive de un `api_channel_id` estable de configuración, aunque el nombre efectivo del nodo todavía no cambie.

---

## 2. Objetivo práctico para la primera iteración

Dejar `IO.api` en un estado donde:

- cada instancia tenga un identificador estable de integración;
- el alta de `ICH` propio use ese identificador estable;
- `listen_addr` siga sirviendo solo para bind/runtime;
- el nodo no quede operativo si falta la configuración mínima de identidad operativa.

---

## 3. Criterio de implementación

### 3.1. Qué cambia

- `own ICH` de `IO.api`:
  - dejar de usar `address = listen_addr`
  - pasar a usar `address = api_channel_id`

### 3.2. Qué no cambia en esta etapa

- el listener HTTP real sigue usando `listen_addr`;
- `thread_id` puede seguir usando material conversacional actual mientras no se defina otra corrección puntual;
- no hace falta rediseñar el deployment ni renombrar inmediatamente todas las instancias.

### 3.3. Regla de transición

Mientras el nodo todavía no esté migrado completamente:

- `listen_addr` puede seguir apareciendo en runtime/logging;
- pero no debe seguir siendo la semilla canónica del `ICH` propio.

---

## 4. Tareas sugeridas

### Bloque A — contrato y documentación de `IO.api`

- [x] Definir en la spec de `IO.api` un campo canónico estable: `api_channel_id`
- [x] Elegir un solo nombre canónico para v1 y evitar mantener ambos salvo alias transitorio
- [ ] Documentar explícitamente que:
  - `listen_addr` es runtime
  - `api_channel_id` es identidad operativa
- [ ] Documentar la configuración mínima para que `IO.api` pueda adquirir su `ICH`:
  - `self_ilk_id`
  - `self_tenant_id`
  - `api_channel_id`
  - `listen_addr`
  - auth/config inbound mínima requerida

### Bloque B — modelo de configuración

- [x] Revisar el shape actual de config de `IO.api`
- [x] Agregar `config.io.api_channel_id` al config efectivo del nodo
- [x] Definir de dónde sale ese valor:
  - `spawn config`
  - `CONFIG_SET`
  - o ambos
- [x] Validar fail-closed si falta ese campo en una instancia que pretende operar
- [x] Definir que el campo es obligatorio para la configuración efectiva operativa

### Bloque C — bootstrap de `ICH` propio

- [x] Cambiar `ensure_io_api_own_ich_registered(...)` para usar:
  - `channel_type = api_channel`
  - `address = api_channel_id`
- [x] Dejar `listen_addr` fuera de la identidad canónica del `ICH`
- [x] Mantener `FAILED_CONFIG` si falla el alta del `ICH`
- [x] Mantener rechazo de hot reload si cambia el identificador estable y no puede asegurarse el nuevo `ICH`
- [x] Detectar cambio de `api_channel_id` además de cambio de `listen`

### Bloque D — lifecycle operativo

- [x] Ajustar el criterio de “nodo operativo”:
  - no operativo si falta `api_channel_id`
  - no operativo si falla `ILK_ADD_CHANNEL`
- [x] Revisar `GET /` / schema / status para que reflejen claramente ese motivo
- [x] Asegurar que `POST /` siga rechazando tráfico real si la instancia no tiene `ICH` propio válido

### Bloque E — semántica de mensajes

- [ ] Revisar `meta.ich` en `IO.api` para que represente la integración estable y no el bind técnico
- [ ] Revisar si hoy `IoContext.entrypoint` en API sigue demasiado acoplado a `listen.address`
- [ ] Decidir si hace falta separar:
  - `entrypoint técnico`
  - `canal lógico estable`
- [ ] Verificar que la identidad externa del sujeto siga resolviéndose por el pipeline de identity, no por el `ICH`

### Bloque F — webhooks/callbacks

- [ ] Revisar si `callback_url` / webhook final pertenece semánticamente a la integración
- [ ] Documentar que ese callback forma parte del canal API configurado
- [ ] Confirmar que no se lo usa como identidad externa ni como sustituto del `ICH`

### Bloque G — compatibilidad y migración

- [ ] Definir estrategia de migración desde instancias actuales basadas implícitamente en `listen_addr`
- [ ] Decidir si habrá:
  - migración automática
  - fallback temporal
  - o corte explícito de compatibilidad
- [ ] Si hay transición, documentar su duración y criterio de salida

### Bloque H — pruebas

- [ ] Test: dos instancias con distinto `api_channel_id` no comparten `ICH`
- [ ] Test: cambiar `listen_addr` sin cambiar `api_channel_id` no cambia identidad canónica del `ICH`
- [ ] Test: falta `api_channel_id` => nodo no operativo
- [ ] Test: `ILK_ADD_CHANNEL` usa identificador estable, no bind técnico
- [ ] Test: `meta.ich` no deriva del caller externo ni del `external_user_id`

---

## 5. Orden recomendado

Orden incremental sugerido:

1. documentación/contrato del identificador estable  
2. shape de config  
3. bootstrap de `ICH` propio  
4. lifecycle no-operativo  
5. semántica de mensajes  
6. migración/compatibilidad  
7. tests

---

## 6. Decisión técnica sugerida para `IO.api`

Si hay que elegir ya una dirección concreta:

- usar **un identificador estable de integración** en config;
- derivar el `ICH` propio desde ese identificador;
- dejar `listen_addr` como detalle operativo de bind;
- no condicionar esta corrección a renombrar primero el nodo físico/lógico.

Eso permite avanzar ahora con el nodo `IO.api` sin bloquearse por el modelo final completo de multiinstancia de deployment.
