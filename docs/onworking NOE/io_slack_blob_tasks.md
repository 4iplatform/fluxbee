# IO Slack + Blob - Plan de tareas (ordenado)

Fecha: 2026-03-26  
Estado: propuesto (sin implementación)

## Objetivo

Incorporar soporte de blobs en `IO.slack` alineado con `text/v1` canónico (`content`, `content_ref`, `attachments`) y reutilizando `fluxbee_sdk::blob` desde una capa genérica en `io-common`.

## Alcance

- `nodes/io/common` (helpers genéricos para payload blob/text).
- `nodes/io/io-slack` (integración inbound/outbound).
- Tests de `io-common` y `io-slack`.
- Sin cambios en `fluxbee_sdk` (salvo necesidad explícita posterior).

## Plan por fases

## Fase 0 - Alineación de contrato (sin código)

- [x] T0.1 Confirmar contrato objetivo de inbound Slack:
  - entrada a router en `payload.type="text"`,
  - `attachments[]` canónico con `BlobRef`,
  - `content_ref` solo cuando aplique por tamaño.
- [x] T0.2 Confirmar política MVP de adjuntos:
  - mimes permitidos,
  - tamaño máximo por archivo,
  - cantidad máxima de adjuntos por mensaje.
- [x] T0.3 Confirmar estrategia de errores para IO:
  - `BLOB_NOT_FOUND`, `BLOB_IO_ERROR`, `BLOB_TOO_LARGE`,
  - `unsupported_attachment_mime`,
  - fallback esperado para casos no recuperables.

Criterio de salida:
- Contrato y límites cerrados para implementar sin ambigüedad.

### Decisión cerrada T0.1 (2026-03-26)

Contrato objetivo para `IO.slack` (inbound hacia router):

1. El payload enviado al router será siempre contrato `text/v1`:
   - `payload.type = "text"`.
2. `attachments` será siempre presente:
   - `attachments: []` si no hay adjuntos,
   - `attachments: [BlobRef, ...]` si hay adjuntos.
3. Contenido textual:
   - usar `content` inline cuando entra en límite de frame,
   - usar `content_ref` cuando el texto excede inline.
4. Exclusión mutua:
   - si existe `content_ref`, se omite `content`.
5. Cada adjunto debe referenciar `active/`:
   - `put` -> `promote` -> recién entonces incluir `BlobRef` en mensaje.
6. El pipeline existente de inbound no cambia:
   - ACK temprano a Slack,
   - dedup/sessionización,
   - identidad vía `io-common` (`lookup -> provision_on_miss -> forward`).

Notas operativas:
- `payload.raw` se mantiene como opcional y acotado para no inflar el frame.
- En errores de blob, el nodo no debe romper el contrato `text/v1`; la política exacta se cierra en T0.3.

### Decisión cerrada T0.2 (2026-03-26)

Política MVP para `IO.slack` (inbound):

1. MIME permitidos:
   - `image/jpeg`
   - `image/png`
   - `image/webp`
   - `image/gif`
   - `application/pdf`
   - `text/plain`
   - `text/markdown`
   - `application/json`
2. Tamaño máximo por archivo:
   - `20 MiB` por adjunto (`20 * 1024 * 1024` bytes).
3. Cantidad máxima de adjuntos por mensaje:
   - `8` adjuntos (alineado con referencia de `max_attachments` en AI specs).
4. Tamaño agregado por mensaje:
   - `40 MiB` sumando todos los adjuntos aceptados.
5. Reglas de aceptación:
   - si un adjunto supera límites de tamaño: `BLOB_TOO_LARGE`.
   - si el MIME no está permitido: `unsupported_attachment_mime`.
   - si supera cantidad máxima: `too_many_attachments`.

Notas de implementación:
- Todos los límites deben ser configurables desde `io-slack`/`io-common` (estos valores son defaults MVP).
- Esta política aplica al flujo inbound Slack -> router.
- El límite general de blob del toolkit (`BlobConfig.max_blob_bytes`) debe mantenerse >= límite por archivo del adapter.

### Decisión cerrada T0.3 (2026-03-27)

Estrategia de errores y fallback para blobs en `IO.slack`:

1. Principio operativo:
   - el ACK a Slack siempre es temprano y nunca depende de procesamiento blob.
2. Mapeo canónico de errores:
   - MIME no permitido -> `unsupported_attachment_mime`.
   - adjunto excede límite -> `BLOB_TOO_LARGE`.
   - falla de descarga/lectura/escritura/promote fs -> `BLOB_IO_ERROR`.
   - referencia a blob no resoluble en consumo outbound -> `BLOB_NOT_FOUND`.
   - exceso de cantidad de adjuntos -> `too_many_attachments`.
3. Fallback inbound (Slack -> router):
   - se procesan adjuntos en forma independiente.
   - si un adjunto falla, se descarta ese adjunto y se continúa con los demás.
   - si queda texto y/o adjuntos válidos, el mensaje se forwardea con el subconjunto válido.
   - si no queda ni texto ni adjuntos válidos, el evento se descarta (sin forward) y se registra warning.
4. Fallback outbound (router -> Slack):
   - si `content_ref` no se puede resolver, usar texto de error operativo breve.
   - si un attachment falla al resolver/enviar, continuar con el resto y registrar warning.
   - si todo falla y no hay texto disponible, publicar un mensaje de error breve en el `reply_target`.
5. Observabilidad mínima obligatoria:
   - counters por `error_code`,
   - cantidad de adjuntos descartados por mensaje,
   - cantidad de mensajes degradados (subconjunto válido),
   - cantidad de mensajes descartados por falta total de contenido útil.

## Fase 1 - Capa genérica en io-common

- [x] T1.1 Crear módulo helper de `text/v1` en `nodes/io/common` para:
  - construir payload con `content` + `attachments`,
  - offload a `content_ref` cuando excede límite inline.
- [x] T1.2 Crear helper de resolución de blobs para consumo outbound:
  - leer `content` inline o resolver `content_ref`,
  - exponer lista de attachments resueltos con metadata útil.
- [x] T1.3 Agregar validaciones comunes:
  - estructura de payload,
  - tamaño/mime/cantidad,
  - mapeo a errores canónicos.
- [x] T1.4 Agregar configuración común:
  - `blob_root`,
  - límites de tamaño/cantidad,
  - timeout de resolve/retry.

Criterio de salida:
- `io-common` permite implementar blobs en cualquier adaptador IO sin duplicar lógica.

### Diseño acordado T1.1 (previo a implementación)

Ubicación propuesta:
- `nodes/io/common/src/text_v1_blob.rs`

Responsabilidad del módulo:
- encapsular construcción de `text/v1` para IO adapters,
- aplicar límites de inline y decidir `content` vs `content_ref`,
- validar `attachments` y devolver errores canónicos normalizados.

Tipos propuestos:

```rust
pub struct IoTextBlobConfig {
    pub max_message_bytes: usize,        // default: 64KB
    pub message_overhead_bytes: usize,   // default: 2048
    pub max_attachments: usize,          // default: 8
    pub max_attachment_bytes: u64,       // default: 20 MiB
    pub max_total_attachment_bytes: u64, // default: 40 MiB
    pub allowed_mimes: Vec<String>,
}

pub struct InboundAttachmentInput {
    pub blob_ref: fluxbee_sdk::blob::BlobRef,
}

pub enum IoBlobContractError {
    BlobTooLarge { actual: u64, max: u64 },      // -> BLOB_TOO_LARGE
    UnsupportedMime { mime: String },            // -> unsupported_attachment_mime
    TooManyAttachments { actual: usize, max: usize }, // -> too_many_attachments
    InvalidTextPayload(String),
    Blob(fluxbee_sdk::blob::BlobError),
}
```

Funciones propuestas:

```rust
pub fn build_text_v1_inbound_payload(
    blob: &fluxbee_sdk::blob::BlobToolkit,
    cfg: &IoTextBlobConfig,
    content: &str,
    attachments: Vec<InboundAttachmentInput>,
) -> Result<serde_json::Value, IoBlobContractError>;
```

Comportamiento esperado:
1. validar cantidad total de adjuntos (`max_attachments`);
2. validar por adjunto `mime` y `size`;
3. validar tamaño agregado (`max_total_attachment_bytes`);
4. construir `TextV1Payload`:
   - `content` inline si entra,
   - `content_ref` si excede inline (vía helper de `BlobToolkit`);
5. retornar JSON payload listo para `InboundProcessor`.

Notas:
- T1.1 no resuelve archivos ni descarga adjuntos; solo arma contrato `text/v1`.
- La resolución para consumo outbound queda en T1.2.

Estado implementación:
- agregado `nodes/io/common/src/text_v1_blob.rs` con:
  - `IoTextBlobConfig`,
  - `IoBlobRuntimeConfig`,
  - `InboundAttachmentInput`,
  - `IoBlobContractError`,
  - `build_text_v1_inbound_payload(...)`,
  - `ResolvedTextV1Payload` + `ResolvedAttachment`,
  - `resolve_text_v1_for_outbound(...)`,
  - `IoBlobContractError::canonical_code()` (mapeo explícito),
  - validaciones de config/límites compartidas.
  - tests unitarios del módulo.
- exportado en `nodes/io/common/src/lib.rs`.
- validación local por `cargo test -p io-common` bloqueada en entorno Windows por dependencias Unix-only de `fluxbee_sdk` (`std::os::fd`, `tokio::net::unix`, `nix::*`).

## Fase 2 - Inbound en IO.slack

- [x] T2.1 Incorporar descarga de adjuntos de Slack (si existen) a `staging/`.
- [x] T2.2 Aplicar `put`/`promote` vía `fluxbee_sdk::blob` para cada adjunto válido.
- [x] T2.3 Armar payload `text/v1` con `attachments[]` canónico.
- [x] T2.4 Usar helper común para decidir `content` vs `content_ref` según tamaño.
- [x] T2.5 Mantener comportamiento existente:
  - ACK temprano,
  - dedup/sessionizer,
  - pipeline de identidad (`lookup -> provision_on_miss -> forward`).

Criterio de salida:
- Un `app_mention` con archivos llega al router con `BlobRef` válido en `attachments[]`.

Estado implementación:
- `io-slack` ahora:
  - detecta `event.files[]`,
  - descarga archivos privados con token bot,
  - aplica filtros de MIME/límites,
  - ejecuta `put_bytes` + `promote`,
  - construye `text/v1` usando `io-common::text_v1_blob::build_text_v1_inbound_payload`.
- fallback inbound aplicado:
  - adjuntos inválidos se descartan individualmente,
  - si falla armado con adjuntos, reintenta `text` sin adjuntos,
  - si no queda ni texto ni adjuntos válidos, descarta evento.
- `raw` enviado en payload quedó reducido a stub acotado (sin volcar evento completo).
- ACK/identidad/dedup conservados:
  - ACK sigue ocurriendo antes de procesamiento downstream,
  - `InboundProcessor` (dedup + identidad) se mantiene sin cambios de contrato.
- decisión operativa:
  - eventos con `files[]` se procesan por camino inmediato (sin sessionizer) para evitar batching de descargas/blob.

## Fase 3 - Outbound en IO.slack

- [x] T3.1 Reemplazar render de texto simple por parsing `text/v1` completo.
- [x] T3.2 Resolver `content_ref` y usarlo como texto principal cuando exista.
- [x] T3.3 Procesar `attachments[]`:
  - resolver archivos,
  - enviar a Slack como adjuntos (según API soportada),
  - manejar fallback de texto si aplica.
- [x] T3.4 Mantener `reply_target` actual (`channel` + `thread_ts`).

Criterio de salida:
- Mensajes del router con `content_ref`/`attachments` se publican correctamente en Slack.

Estado implementación:
- outbound ahora usa `resolve_text_v1_for_outbound(...)` (`io-common`):
  - texto desde `content` o desde blob (`content_ref`),
  - resolución de `attachments[]` a paths locales.
- envío a Slack:
  - texto por `chat.postMessage`,
  - adjuntos por flujo externo:
    - `files.getUploadURLExternal`
    - upload binario al `upload_url`
    - `files.completeUploadExternal`
    (al mismo `channel/thread_ts`).
- fallback operativo:
  - error en resolución `text/v1` -> mensaje de error corto en Slack,
  - fallo por adjunto individual -> se loguea warning y continúa con los demás,
  - si no se pudo enviar nada -> mensaje final de fallback.

## Fase 4 - Tests

- [ ] T4.1 Unit tests `io-common`:
  - validación `text/v1`,
  - offload a `content_ref`,
  - errores de límite/mime.
- [ ] T4.2 Unit/integration tests `io-slack` inbound:
  - mensaje sin adjuntos,
  - mensaje con adjuntos válidos,
  - adjunto inválido (mime/tamaño).
- [ ] T4.3 Unit/integration tests `io-slack` outbound:
  - `content` inline,
  - `content_ref`,
  - `attachments[]`.
- [ ] T4.4 Test de no regresión:
  - dedup,
  - ACK no bloqueante,
  - identidad degradada (`src_ilk=null`) intacta.

Criterio de salida:
- Cobertura suficiente para cambiar con seguridad sin romper flujo actual.

## Fase 5 - Cierre operativo

- [ ] T5.1 Actualizar docs IO (`docs/io/io-slack.md` y/o runbook) con blobs.
- [ ] T5.2 Definir checklist de deploy/validación en ambiente real.
- [ ] T5.3 Registrar límites/flags de config recomendados por defecto.

Criterio de salida:
- Equipo de operación puede desplegar y validar blobs en Slack con pasos claros.

## Orden de ejecución recomendado

1. Fase 0
2. Fase 1
3. Fase 2
4. Fase 3
5. Fase 4
6. Fase 5

---

## Addendum 2026-04-01 (enforcement comun `text/v1`)

Estado actualizado de implementacion:

- La decision `content` vs `content_ref` por limite de tamano ya no depende de `io-slack`.
- El enforcement se ejecuta en `io-common::InboundProcessor` para payloads inbound `type="text"`.
- `io-slack` ahora construye payload base `text/v1` (texto + adjuntos) y delega la normalizacion final a `io-common`.

Implicancias:

- Este punto reemplaza cualquier referencia previa que sugiera que `io-slack` decide localmente el offload por tamano.
- El comportamiento esperado para adapters IO es:
  1. mapear evento externo -> payload `text/v1` base,
  2. delegar normalizacion/offload a `io-common`.

Logging operativo esperado (comun):

- `debug`: `inbound text/v1 normalized` con `offload_to_blob=true|false`.
- `info`: `inbound text/v1 processed as blob` cuando aplica offload.
- `warn`: errores de normalizacion con `canonical_code`.

Limitacion de validacion en canal Slack:

- El comportamiento de offload para payloads de texto extremos debe validarse de forma determinística con `io-sim`.
- En Slack real, `chat.postMessage` trunca texto por encima de 40.000 caracteres, por lo que no sirve como prueba robusta de umbral interno `>64KB`.
  - Fuente: https://docs.slack.dev/reference/methods/chat.postMessage/ (Truncating content).
