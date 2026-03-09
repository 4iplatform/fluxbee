# Blob SDK - Ejemplos por nodo (IO/AI/WF)

Referencia de uso del contrato `text/v1` con `fluxbee_sdk`.

Nota de alcance:
- este documento es de ejemplos/contratos de uso.
- la lista activa de tareas está en `docs/onworking/blob_tasks.md`.

## 1) IO recibe archivo externo y envía a AI

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit};
use fluxbee_sdk::payload::TextV1Payload;

let blob = BlobToolkit::new(BlobConfig {
    max_blob_bytes: Some(100 * 1024 * 1024), // 100MB
    ..BlobConfig::default()
})?;

// 1) guardar archivo en staging
let blob_ref = blob.put("/tmp/incoming/factura.png".as_ref(), "Factura Marzo 2026.png")?;

// 2) promover a active cuando está listo
blob.promote(&blob_ref)?;

// 3) armar payload text/v1
let payload = TextV1Payload::new("Mirá esta factura", vec![blob_ref]);
let payload_json = payload.to_value()?;
```

## 2) AI genera documento y responde a IO

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit};
use fluxbee_sdk::payload::TextV1Payload;

let blob = BlobToolkit::new(BlobConfig {
    max_blob_bytes: Some(100 * 1024 * 1024),
    ..BlobConfig::default()
})?;

let pdf_bytes: Vec<u8> = generar_pdf();
let pdf_ref = blob.put_bytes(&pdf_bytes, "respuesta.pdf", "application/pdf")?;
blob.promote(&pdf_ref)?;

let payload = TextV1Payload::new("Adjunto el PDF", vec![pdf_ref]);
let payload_json = payload.to_value()?;
```

## 3) WF mensaje largo -> `content_ref` automático

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit};

let blob = BlobToolkit::new(BlobConfig {
    max_blob_bytes: Some(100 * 1024 * 1024),
    ..BlobConfig::default()
})?;
let texto_largo = construir_resumen_largo();

// Si supera límite estimado de mensaje, crea content_ref automáticamente.
let payload = blob.build_text_v1_payload(&texto_largo, vec![])?;
let payload_json = payload.to_value()?;
```

## 4) Consumidor (IO/AI/WF) resuelve attachments

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit, ResolveRetryConfig};
use fluxbee_sdk::payload::TextV1Payload;

let blob = BlobToolkit::new(BlobConfig {
    max_blob_bytes: Some(100 * 1024 * 1024),
    ..BlobConfig::default()
})?;
let payload: TextV1Payload = TextV1Payload::from_value(&payload_json)?;

for attachment in payload.attachments {
    // single-isla: resolve directo
    let path = blob.resolve(&attachment);
    // multi-isla con sync: resolve_with_retry
    let _path_retry = blob
        .resolve_with_retry(&attachment, ResolveRetryConfig::default())
        .await?;
    let bytes = std::fs::read(path)?;
    procesar(bytes);
}
```

## 5) Reglas operativas

- Siempre `put`/`put_bytes` -> `promote` -> enviar mensaje.
- No referenciar blobs en `staging/`.
- `attachments` siempre array (vacío si no hay adjuntos).
- Si hay `content_ref`, no incluir `content`.
- En multi-isla, preferir publicación con confirmación (`publish_blob_and_confirm`) antes de enviar el mensaje.
- El consumidor debe mantener `resolve_with_retry` como red de seguridad ante convergencia tardía.
- Configurar `BlobConfig.max_blob_bytes` según política de canal para recibir `BLOB_TOO_LARGE` de forma explícita.

## 6) Propuesta v2.x (pendiente): llamada unificada en SDK con confirmación

Objetivo:
- el nodo productor (IO/AI/WF) usa una sola API de SDK para publicar blob y confirmar propagación antes de emitir el mensaje.

Contrato propuesto:
- `publish_blob_and_confirm(...)`:
  - `put/put_bytes`
  - `promote`
  - `SYSTEM_SYNC_HINT` al orchestrator para canal `blob`
  - espera confirmación de disponibilidad en hives destino (o timeout)
  - retorna `BlobRef` listo para adjuntar

Pseudocódigo:

```rust
// API propuesta (nombres tentativos)
let publish = blob.publish_blob_and_confirm(&sender, &mut receiver, PublishBlobRequest {
    data: &pdf_bytes,
    filename_original: "respuesta.pdf",
    mime: "application/pdf",
    targets: vec!["worker-220".into()],
    wait_for_idle: true,
    timeout_ms: 30_000,
}).await?;

let payload = TextV1Payload::new("Adjunto el PDF", vec![publish.blob_ref]);
send_text_v1(payload).await?;
```

Garantía buscada:
- no enviar mensaje con `blob_ref` hasta que la propagación esté consolidada para destinos declarados.
- fallback explícito: timeout/retry/error tipado (sin comportamiento implícito).
- estados esperados del hint: `ok`, `sync_pending`, `error`.
