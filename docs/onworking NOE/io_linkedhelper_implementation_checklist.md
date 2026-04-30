# Checklist de implementación — `IO.linkedhelper`

> Checklist operativo alineado a las definiciones vigentes en:
>
> - `docs/io/io-linked-helper-contratos.md`
> - `docs/io/io-linked-helper-protocolo.md`
> - `docs/io/io-linked-helper-responsabilidades.md`
> - `docs/io/io-nodes-monitoreo-ich-general.md`

---

## 1. Definiciones base ya asumidas

- [x] `profile_create` reemplaza al enfoque anterior basado en `avatar_create`
- [x] `profile_update` queda fuera del MVP
- [x] el nodo crea ILK provisorio tipo `agent`
- [x] el adapter no recibe ni usa ILKs provisorios
- [x] `conversation_message` solo aplica a profiles listos y con automatización LH habilitada
- [x] el nodo debe monitorear estados de sus ICHs propios
- [x] la correlación efectiva del nodo se toma como `adapter_id + event_id`
- [x] el adapter es responsable de construir `external_profile_id` canónico

---

## 2. Contrato y mecanismo externo

- [x] Definir primer shape implementable de request/response HTTP
- [x] Definir headers mínimos de autenticación por installation key
- [x] Definir envelope mínimo para `heartbeat`
- [x] Definir envelope mínimo para `profile_create`
- [ ] Definir envelope mínimo para `conversation_message`
- [x] Definir envelope mínimo para `ack`
- [x] Definir envelope mínimo para `result`
- [x] Definir envelope mínimo para `profile_ready`
- [ ] Definir envelope mínimo para `automation_enabled`
- [ ] Definir envelope mínimo para `automation_disabled`

Nota:

- el JSON exacto sigue siendo tentativo;
- este bloque apunta a fijar una primera versión implementable, no el contrato final definitivo.

---

## 3. Skeleton del nodo

- [x] Crear crate/bin `IO.linkedhelper`
- [x] Agregar schema base del nodo
- [x] Definir config mínima inicial
- [x] Implementar arranque HTTP único
- [x] Implementar autenticación mínima por installation key
- [x] Implementar parsing inicial de batch

Nota:

- el skeleton actual compila y expone `GET /schema`, `POST /v1/poll` y control-plane básico;
- por ahora el batch solo valida/authentica y responde `heartbeat` o `ack/result` mínimos;
- todavía no hay lógica real de `profile_create`, `conversation_message`, monitoreo de SHM ni estado durable.

---

## 4. Estado durable mínimo

- [x] Persistir mapping `adapter_id ↔ installation key` o referencia equivalente
- [ ] Persistir mapping `adapter ↔ profiles descubiertos`
- [ ] Persistir mapping `external_profile_id ↔ ILK`
- [ ] Persistir listado de ILKs provisorios pendientes
- [ ] Persistir último estado observado por ICH propio
- [x] Persistir cambios pendientes de entregar al adapter
- [x] Persistir cola o reconstrucción de resultados por adapter

Nota:

- el nodo ya persiste una store JSON durable propia con adapters sincronizados, referencia de installation key, metadata básica de poll y pending deliveries por adapter;
- el poll/heartbeat ya drena pending deliveries desde esa store;
- todavía no hay productores reales de profiles, ILKs provisorios ni estados de ICH.
- esta store local cumple para el MVP, pero no debe considerarse el destino final de producción;
- queda asentado que a futuro habrá que migrar a una persistencia más robusta para estado operativo mutable, colas pendientes y updates frecuentes del canal.

---

## 5. Flujo de `profile_create`

- [x] Recibir y validar `profile_create`
- [ ] Crear ILK provisorio tipo `agent`
- [x] Asociar el profile al tenant host/default
- [x] Guardar el profile como pendiente de promoción
- [x] No devolver el ILK provisorio al adapter
- [x] Registrar `ack` y resultado pendiente cuando corresponda

Nota importante:

- el flujo actual ya hace lookup/provision y persiste el profile pendiente;
- pero `ILK_PROVISION` en core hoy crea ILKs temporales con `ilk_type = "human"` hardcodeado;
- por eso el ítem "Crear ILK provisorio tipo `agent`" sigue abierto y requiere definición/cambio de core o un camino alternativo explícito.

---

## 6. Monitoreo de promoción de ILK

- [ ] Implementar observación de identity SHM para profiles pendientes
- [ ] Detectar paso a estado utilizable/`complete`
- [ ] Emitir `profile_ready` al adapter correcto
- [ ] Limpiar el estado pendiente del profile promovido

---

## 7. Monitoreo de ICH propios

- [ ] Definir cómo el nodo identifica cuáles ICHs considera propios
- [ ] Observar cambios de estado de esos ICHs
- [ ] Persistir el último estado relevante por ICH
- [ ] Colapsar cambios pendientes por ICH al último estado relevante
- [ ] Emitir `automation_enabled` al adapter correcto
- [ ] Emitir `automation_disabled` al adapter correcto

---

## 8. Gating por profile

- [ ] Bloquear `conversation_message` para profiles sin ILK utilizable
- [ ] Bloquear `conversation_message` para ICH LH con automatización desactivada
- [ ] Mantener el bloqueo por profile sin afectar a otros profiles
- [ ] Devolver `blocked_profile` o error equivalente cuando corresponda

---

## 9. Flujo de `conversation_message`

- [ ] Validar shape mínimo del evento
- [ ] Resolver identidad mínima del contacto
- [ ] Tratar `contact_external_composite_id` como `external_id` canónico opaco
- [ ] Resolver routing interno a Fluxbee
- [ ] Emitir el mensaje interno solo si el profile está habilitado
- [ ] Devolver `conversation_processed` o error equivalente

Nota MVP:

- si falla la resolución mínima del contacto, el nodo devuelve error y no agrega waits complejos.

---

## 10. `system_alert`

- [ ] Decidir si entra o no en el MVP efectivo
- [ ] Si entra, definir el primer subset útil
- [ ] Si entra, definir su payload mínimo

---

## 11. Operación y observabilidad

- [ ] Logging mínimo del canal
- [ ] Logging de profiles provisorios/promovidos
- [ ] Logging de cambios por ICH
- [ ] Logging de cola pendiente por adapter
- [ ] Métricas mínimas del nodo si el patrón del repo lo justifica

---

## 12. Verificación E2E

- [ ] E2E `profile_create` -> ILK provisorio -> promoción -> `profile_ready`
- [ ] E2E `conversation_message` con profile habilitado
- [ ] Verificar bloqueo de profile no listo
- [ ] Verificar bloqueo de automatización desactivada por ICH
- [ ] Verificar entrega diferida por heartbeat/beacon
