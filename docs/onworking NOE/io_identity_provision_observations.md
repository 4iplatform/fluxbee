# IO Identity Provision - Observaciones y Ajustes Pendientes

Fecha: 2026-03-17
Scope: pruebas E2E con `io-sim` + frontdesk + `SY.identity` en baseline provision-on-miss.

---

## 1) Estado actual validado

- `io-sim` en baseline provision-on-miss logra pedir identidad provisoria a `SY.identity`.
- `io-sim` envía mensajes al router con `dst=resolve`.
- El ILK devuelto para el mismo `(channel, external_id)` es estable (idempotente).
- Se eliminó la carrera principal de lectura en sim (ya no hay doble consumidor simultáneo en la práctica de request+reply del mismo trace).

---

## 1.1) Hallazgo operativo nuevo: EACCES en lookup SHM

Observado en logs de IO:
- `identity lookup error; treating as miss ... EACCES: Permission denied`

Interpretacion:
- `SY.identity` responde a provision, pero el proceso IO no puede leer `/dev/shm/jsr-identity-<island>`.
- No es un bug del pipeline de IO; es un problema de permisos del entorno/runtime user.

Implicancia:
- Puede haber reprovision en cada mensaje tras restart, aunque el mapping ICH->ILK exista.

---

## 2) Comportamientos observados

### 2.1 Provision exitoso pero no derivación a frontdesk con `dst=resolve`

Observado:
- `identity provisioned on miss ... src_ilk=ilk:...`
- respuesta vacía desde router para ese `trace_id`.

Interpretación:
- El router no está resolviendo ruta hacia frontdesk para ese caso.
- Logs de `sy-config-routes` muestran sólo regla activa `WF.echo.*`.

Esperable:
- Si existe regla/policy para ese `src_ilk`/tipo `user`, resolver a `SY.frontdesk.gov@motherbee`.
- Si no existe, resolve falla (comportamiento actual).

### 2.2 Miss repetido en lookup SHM para mismo sender

Observado:
- Cada mensaje muestra `identity provisioned on miss` (no aparece hit de lookup).
- El ILK devuelto es siempre el mismo.

Interpretación:
- `lookup` local SHM no está encontrando la identidad.
- `provision` es idempotente y devuelve el mapping existente.

Esperable ideal:
- Primer mensaje: miss -> provision -> ILK.
- Mensajes siguientes: hit en lookup SHM (sin reprovision).

Estado:
- Funcionalmente tolerado (porque provision recupera mismo ILK).
- No ideal desde performance/latencia.

---

## 3) Causas técnicas probables

1. Reglas de routing/policy incompletas para derivar a frontdesk en `resolve`.
2. Lookup SHM no alineado con estado real de identity (sync/layout/refresh/island).
3. Aunque `ISLAND_ID=motherbee` se usó, persiste miss en lookup; revisar contrato exacto de SHM mapeado por identity.

---

## 4) Comportamiento esperado (target)

1. `io-sim` recibe mensaje.
2. Resuelve identidad por SHM:
   - si hit: usa `src_ilk` directo.
   - si miss: hace `ILK_PROVISION`.
3. Tras provision exitoso, próximos mensajes para mismo sender deben dar hit en lookup (sin reprovision).
4. Con `dst=resolve`, router debe derivar por reglas/policy al destino esperado (ej. frontdesk), no responder vacío.

---

## 5) Ajustes pendientes (orden sugerido)

1. Routing/policy:
   - agregar/activar regla de resolve para frontdesk en este flujo de prueba.
2. Diagnóstico SHM lookup:
   - validar por qué no hay hit luego de provision.
   - revisar refresh/lectura de `jsr-identity-<island>`.
3. Mitigación temporal (si se necesita continuidad de pruebas):
   - cache local corto `channel+external_id -> ilk` en IO para evitar reprovision repetido cuando SHM miss persiste.
4. Observabilidad:
   - logs más explícitos de lookup hit/miss + source (`shm|provision|cache`) para trazabilidad operativa.

---

## 6) Nota operativa

- El flujo de identidad provisoria quedó operativo.
- El cuello actual para pruebas E2E completas es principalmente de resolución en router/policy y de hit SHM post-provision.
