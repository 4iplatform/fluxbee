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
- [x] Definir envelope mínimo para `conversation_message`
- [x] Definir envelope mínimo para `ack`
- [x] Definir envelope mínimo para `result`
- [x] Definir envelope mínimo para `profile_ready`
- [x] Definir envelope mínimo para `automation_enabled`
- [x] Definir envelope mínimo para `automation_disabled`

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
- el batch ya autentica, procesa `profile_create`, procesa `conversation_message` y responde `heartbeat` / `ack` / `result`;
- el monitoreo de SHM y la store durable ya existen en forma MVP.

---

## 4. Estado durable mínimo

- [x] Persistir mapping `adapter_id ↔ installation key` o referencia equivalente
- [x] Persistir mapping `adapter ↔ profiles descubiertos`
- [x] Persistir mapping `external_profile_id ↔ ILK`
- [x] Persistir listado de ILKs provisorios pendientes
- [x] Persistir último estado observado por ICH propio
- [x] Persistir cambios pendientes de entregar al adapter
- [x] Persistir cola o reconstrucción de resultados por adapter

Nota:

- el nodo ya persiste una store JSON durable propia con adapters sincronizados, referencia de installation key, metadata básica de poll y pending deliveries por adapter;
- el poll/heartbeat ya drena pending deliveries desde esa store;
- el nodo ya produce y persiste estado real de profiles, ILKs observados y estados de ICH propios;
- esta store local cumple para el MVP, pero no debe considerarse el destino final de producción;
- queda asentado que a futuro habrá que migrar a una persistencia más robusta para estado operativo mutable, colas pendientes y updates frecuentes del canal.

---

## 5. Flujo de `profile_create`

- [x] Recibir y validar `profile_create`
- [x] Crear ILK provisorio tipo `agent`
- [x] Asociar el profile al tenant host/default
- [x] Guardar el profile como pendiente de promoción
- [x] No devolver el ILK provisorio al adapter
- [x] Registrar `ack` y resultado pendiente cuando corresponda

---

## 6. Monitoreo de promoción de ILK

- [x] Implementar observación de identity SHM para profiles pendientes
- [x] Detectar paso a estado utilizable/`complete`
- [x] Emitir `profile_ready` al adapter correcto
- [x] Limpiar el estado pendiente del profile promovido

Nota:

- en esta etapa el monitoreo de promoción corre de forma oportunista durante los polls/beacons del adapter;
- no hay todavía watcher dedicado ni loop separado de observación continua.

---

## 7. Monitoreo de ICH propios

- [x] Definir cómo el nodo identifica cuáles ICHs considera propios
- [x] Observar cambios de estado de esos ICHs
- [x] Persistir el último estado relevante por ICH
- [x] Colapsar cambios pendientes por ICH al último estado relevante
- [x] Emitir `automation_enabled` al adapter correcto
- [x] Emitir `automation_disabled` al adapter correcto

Nota:

- en esta etapa el nodo considera propios los ICHs asociados a profiles descubiertos y persistidos por `IO.linkedhelper`;
- la observación también corre de forma oportunista durante los polls/beacons del adapter;
- el colapso se hace por `ich_id`, manteniendo solo el último estado relevante pendiente.

---

## 8. Gating por profile

- [x] Bloquear `conversation_message` para profiles sin ILK utilizable
- [x] Bloquear `conversation_message` para ICH LH con automatización desactivada
- [x] Mantener el bloqueo por profile sin afectar a otros profiles
- [x] Devolver `blocked_profile` o error equivalente cuando corresponda

---

## 9. Flujo de `conversation_message`

- [x] Validar shape mínimo del evento
- [x] Resolver identidad mínima del contacto
- [x] Tratar `contact_external_composite_id` como `external_id` canónico opaco
- [x] Resolver routing interno a Fluxbee
- [x] Emitir el mensaje interno solo si el profile está habilitado
- [x] Devolver `conversation_processed` o error equivalente

Nota MVP:

- si falla la resolución mínima del contacto, el nodo devuelve error y no agrega waits complejos.
- el contenido hoy acepta string no vacío como texto o un objeto no vacío como payload ya estructurado;
- el routing interno exige `dst_node` configurado en el adapter;
- el monitoreo de automatización sigue siendo oportunista por poll/beacon, así que el gating depende del último estado durable observado por el nodo.

---

## 10. `system_alert`

- [ ] Decidir si entra o no en el MVP efectivo
- [ ] Si entra, definir el primer subset útil
- [ ] Si entra, definir su payload mínimo

---

## 11. Operación y observabilidad

- [x] Logging mínimo del canal
- [x] Logging de profiles provisorios/promovidos
- [x] Logging de cambios por ICH
- [ ] Logging de cola pendiente por adapter
- [ ] Métricas mínimas del nodo si el patrón del repo lo justifica

Nota:

- la observabilidad actual es event-driven, no periódica por defecto;
- los hitos operativos relevantes salen en `INFO` sin depender de configurar variables extra;
- el detalle rutinario por poll quedó en `DEBUG` para evitar ruido en consola en una instalación normal.

---

## 12. Verificación E2E

- [ ] E2E `profile_create` -> ILK provisorio -> promoción -> `profile_ready`
- [ ] E2E `conversation_message` con profile habilitado
- [ ] Verificar bloqueo de profile no listo
- [ ] Verificar bloqueo de automatización desactivada por ICH
- [ ] Verificar entrega diferida por heartbeat/beacon
