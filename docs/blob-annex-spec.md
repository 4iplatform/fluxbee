# Anexo Técnico: Blob Storage y Attachments

**Estado:** v1.1
**Fecha:** 2026-03-09
**Audiencia:** Desarrolladores de nodos, desarrolladores de `fluxbee_sdk`
**Complementa:** `02-protocolo.md` §4 y §11, `07-operaciones.md`, `blob_tasks.md`

---

## 1. Principio

Todo archivo binario (imagen, PDF, audio, documento, generado) viaja **fuera del mensaje**.
El mensaje solo lleva una referencia liviana (`BlobRef`).
Todos los nodos acceden al mismo repositorio blob vía filesystem local.

**No existe transferencia de archivos por mensaje.** Si un nodo necesita enviar un archivo a otro nodo, lo escribe al blob storage y envía la referencia. El router no inspecciona ni modifica blobs ni payload.

---

## 2. Repositorio Blob

### 2.1 Layout

```
/var/lib/fluxbee/blob/
├── staging/           ← workspace de nodos: archivos en progreso, no listos para consumo
│   └── a1/
│       └── factura_a1b2c3d4e5f6a7b8.png
├── active/            ← archivos listos, referenciados por mensajes
│   └── a1/
│       └── factura_a1b2c3d4e5f6a7b8.png
└── <futuro: GC opera sobre active/ por tiempo>
```

**`staging/`** — Directorio de trabajo de los nodos. Cada nodo escribe aquí los archivos que está procesando, generando, descargando o transformando. Un archivo en staging no está listo para ser referenciado por un mensaje. Ejemplos:

- **Nodo IO:** descargando imagen de WhatsApp (download en curso)
- **Nodo AI:** generando un PDF o imagen (rendering en curso)
- **Nodo WF:** ensamblando un reporte con múltiples fuentes
- **Cualquier nodo:** convirtiendo formato, redimensionando, procesando

**`active/`** — Archivos consolidados y referenciables. Cuando el nodo productor decide que el archivo está listo, lo promueve de `staging/` a `active/` (rename atómico). A partir de ese momento:

- El archivo puede ser referenciado en un mensaje (`BlobRef`)
- Otros nodos pueden leerlo
- Syncthing lo replica a otras islas (si está habilitado)

**`staging/` no se replica.** Es local a cada isla. Solo `active/` es visible para Syncthing.

**Subdirectorio prefix** — Primeros 2 caracteres del hash (extraídos del `blob_name`). Distribuye archivos en hasta 256 subdirectorios para evitar directorios con miles de entradas.

### 2.2 Naming de archivos

```
<nombre_fuente_truncado>_<hash16>.<ext>
```

| Componente | Regla | Ejemplo |
|------------|-------|---------|
| `nombre_fuente` | Nombre original del archivo, sanitizado y truncado a **128 chars** max | `factura-marzo-2026` |
| `_` | Separador fijo | `_` |
| `hash16` | Primeros 16 caracteres hex del SHA-256 del contenido | `a1b2c3d4e5f6a7b8` |
| `.ext` | Extensión original del archivo fuente, lowercase | `.png` |

**Nombre completo máximo:** 128 + 1 + 16 + 1 + ext(max 10) = **156 chars**

**Límites de filesystem:**

| Filesystem | NAME_MAX | Nuestro máximo | Margen |
|------------|----------|----------------|--------|
| ext4 / xfs / btrfs | 255 bytes | 156 bytes | 99 bytes |
| eCryptFS | 143 bytes | 156 bytes | ⚠️ excede |
| NTFS (referencia) | 255 chars | 156 chars | 99 chars |

> **Nota:** Si el entorno usa eCryptFS (poco común en servers), reducir truncamiento de nombre fuente a 80 chars. El default de 128 es seguro para ext4/xfs/btrfs que son los filesystems estándar de producción.

### 2.3 Sanitización del nombre fuente

El productor (`fluxbee_sdk` blob module) sanitiza el nombre original antes de usarlo:

```
1. Extraer nombre sin extensión y extensión por separado
2. Reemplazar caracteres no permitidos [^a-zA-Z0-9._-] → "_"
3. Colapsar underscores múltiples → uno solo
4. Truncar nombre a 128 chars (cortar, no rechazar)
5. Lowercase la extensión
6. Si nombre queda vacío después de sanitizar → usar "blob"
```

**Caracteres permitidos en nombre:** `a-z A-Z 0-9 . _ -`

Esto garantiza compatibilidad con Linux, macOS, Windows (para inspección manual) y Syncthing.

---

## 3. BlobRef — Estructura canónica

El `blob_name` es el identificador único del blob. No hay campo hash separado — el hash está embebido en el nombre y se extrae cuando se necesita (para derivar el subdirectorio prefix).

```json
{
  "type": "blob_ref",
  "blob_name": "factura-marzo-2026_a1b2c3d4e5f6a7b8.png",
  "size": 5242880,
  "mime": "image/png",
  "filename_original": "Factura Marzo 2026.png",
  "spool_day": "2026-02-24"
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `type` | string | Sí | Siempre `"blob_ref"` |
| `blob_name` | string | Sí | Nombre canónico del archivo en el repositorio. Es el ID del blob. Contiene hash16 embebido |
| `size` | integer | Sí | Tamaño en bytes del archivo original |
| `mime` | string | Sí | MIME type (`image/png`, `application/pdf`, etc.) |
| `filename_original` | string | Sí | Nombre original del archivo tal como lo envió el usuario/sistema. Sin sanitizar. Para display |
| `spool_day` | string | Sí | Fecha ISO del día en que se creó. Para GC futuro por tiempo |

**Resolución de path desde BlobRef:**

El prefix de subdirectorio se extrae del `blob_name`: se toman los 2 caracteres inmediatamente después del último `_` (que corresponden a los primeros 2 hex del hash).

```
blob_name = "factura-marzo-2026_a1b2c3d4e5f6a7b8.png"
                                  ^^
                                  prefix = "a1"

→ path = /var/lib/fluxbee/blob/active/a1/factura-marzo-2026_a1b2c3d4e5f6a7b8.png
```

**Compatibilidad con spec existente:** el campo `blob_id` de `02-protocolo.md` §11.2 se reemplaza por `blob_name`. El `blob_name` cumple la misma función de identificador único.

---

## 4. Contrato de Payload: `text/v1`

La spec (`02-protocolo.md` §4) establece que la estructura interna del payload es libre y la definen los nodos. Esto no cambia.

Lo que se define aquí es el **contrato `text/v1`**: la estructura estándar para mensajes conversacionales entre nodos (IO↔AI, AI↔IO, etc.). Es una convención entre nodos, no una imposición del router.

### 4.1 Estructura del contrato `text/v1`

```json
{
  "payload": {
    "type": "text",
    "content": "string | vacío",
    "attachments": [ BlobRef, ... ]
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `type` | string | Sí | `"text"` — identifica este contrato |
| `content` | string | Sí | Texto del mensaje. Puede ser vacío (`""`) si solo hay attachments |
| `attachments` | array | Sí | Lista de `BlobRef`. Array vacío `[]` si no hay attachments |

**Reglas:**

- `attachments` siempre presente, array vacío si no hay adjuntos.
- Cada attachment es un `BlobRef` completo apuntando a un archivo en `active/`.
- El router no valida esta estructura. Es contrato entre nodos.
- Nodos que implementan otros flujos (comandos de sistema, eventos internos, workflows) pueden usar su propia estructura de payload.

### 4.2 Ejemplos

**Texto con attachments:**

```json
{
  "routing": { "src": "io.whatsapp@isla1", "dst": "ai.openai@isla1", "ttl": 16, "trace_id": "..." },
  "meta": { "type": "user", "src_ilk": "io", "ich": "ch-001", "ctx": "ctx-abc", "ctx_seq": 47 },
  "payload": {
    "type": "text",
    "content": "Mirá esta factura",
    "attachments": [
      {
        "type": "blob_ref",
        "blob_name": "factura-marzo-2026_a1b2c3d4e5f6a7b8.png",
        "size": 5242880,
        "mime": "image/png",
        "filename_original": "Factura Marzo 2026.png",
        "spool_day": "2026-02-24"
      }
    ]
  }
}
```

**Solo texto:**

```json
{
  "payload": {
    "type": "text",
    "content": "Hola, necesito ayuda",
    "attachments": []
  }
}
```

**Solo attachments (sin texto):**

```json
{
  "payload": {
    "type": "text",
    "content": "",
    "attachments": [
      {
        "type": "blob_ref",
        "blob_name": "foto1_b2c3d4e5f6a7b8c9.jpg",
        "size": 3145728,
        "mime": "image/jpeg",
        "filename_original": "IMG_20260224.jpg",
        "spool_day": "2026-02-24"
      },
      {
        "type": "blob_ref",
        "blob_name": "foto2_d4e5f6a7b8c9d0e1.jpg",
        "size": 2097152,
        "mime": "image/jpeg",
        "filename_original": "IMG_20260224_2.jpg",
        "spool_day": "2026-02-24"
      }
    ]
  }
}
```

### 4.3 Regla de 64KB

El límite de 64KB aplica al **mensaje JSON completo** (routing + meta + payload serializado). Como los blobs viajan fuera del mensaje, un mensaje con 10 attachments sigue siendo liviano (cada `BlobRef` ocupa ~150 bytes).

Si el campo `content` por sí solo excede 64KB (ej: paste enorme de texto), el contenido textual también se promueve a blob:

```json
{
  "payload": {
    "type": "text",
    "content_ref": {
      "type": "blob_ref",
      "blob_name": "paste-largo_c3d4e5f6a7b8c9d0.txt",
      "size": 150000,
      "mime": "text/plain",
      "filename_original": "paste.txt",
      "spool_day": "2026-02-24"
    },
    "attachments": []
  }
}
```

Cuando `content_ref` está presente, `content` se omite. El consumidor lee el texto del blob. Regla simple: si cabe inline, va inline. Si no cabe, va como `blob_ref`. El productor decide.

---

## 5. Blob Module — `fluxbee_sdk`

El manejo de blobs es un módulo dentro del paquete `fluxbee_sdk`. Todos los nodos que necesiten leer o escribir blobs usan este módulo.

### 5.1 API pública

```rust
// fluxbee_sdk::blob

/// Configuración del módulo blob
pub struct BlobConfig {
    pub blob_root: PathBuf,       // /var/lib/fluxbee/blob
    pub name_max_chars: usize,    // default: 128
    pub max_blob_bytes: Option<u64>, // default: Some(100MB), None => sin límite
}

/// Referencia a un blob en el repositorio (serializable a JSON)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BlobRef {
    #[serde(rename = "type")]
    pub ref_type: String,           // siempre "blob_ref"
    pub blob_name: String,          // nombre canónico (ID del blob)
    pub size: u64,
    pub mime: String,
    pub filename_original: String,  // nombre original sin sanitizar
    pub spool_day: String,          // "2026-02-24"
}

/// Estadísticas de un blob
pub struct BlobStat {
    pub size_on_disk: u64,
    pub exists: bool,
    pub path: PathBuf,
}

/// Configuración de retry para resolve en multi-isla
pub struct ResolveRetryConfig {
    pub max_wait_ms: u64,         // default: 5000
    pub initial_delay_ms: u64,    // default: 100
    pub backoff_factor: f64,      // default: 2.0
}

impl BlobToolkit {
    /// Crear toolkit con configuración
    pub fn new(config: BlobConfig) -> Result<Self>;

    /// Escribir archivo al blob storage (staging/).
    /// Calcula hash, sanitiza nombre, crea subdirectorio, copia archivo.
    /// Retorna BlobRef. El archivo queda en staging/ hasta promote.
    pub fn put(&self, source_path: &Path, original_filename: &str) -> Result<BlobRef>;

    /// Escribir bytes al blob storage (staging/).
    /// Para nodos que generan contenido en memoria.
    pub fn put_bytes(&self, data: &[u8], original_filename: &str, mime: &str) -> Result<BlobRef>;

    /// Promover blob de staging/ a active/.
    /// Rename atómico. Llamar cuando el archivo está listo para consumo.
    /// El productor decide cuándo promover.
    pub fn promote(&self, blob_ref: &BlobRef) -> Result<()>;

    /// Resolver path completo de un blob en active/.
    /// Retorna el path inmediatamente sin verificar existencia.
    pub fn resolve(&self, blob_ref: &BlobRef) -> PathBuf;

    /// Resolver path con retry y backoff.
    /// Para entornos multi-isla donde Syncthing puede no haber replicado aún.
    /// Reintenta hasta max_wait_ms con backoff exponencial.
    pub async fn resolve_with_retry(
        &self,
        blob_ref: &BlobRef,
        config: ResolveRetryConfig,
    ) -> Result<PathBuf>;

    /// Verificar si un blob existe en active/.
    pub fn exists(&self, blob_ref: &BlobRef) -> bool;

    /// Obtener metadata de un blob en active/.
    pub fn stat(&self, blob_ref: &BlobRef) -> Result<BlobStat>;

    /// Extraer prefix de subdirectorio desde blob_name.
    /// Toma los 2 chars después del último '_' (primeros 2 hex del hash).
    pub fn prefix(blob_name: &str) -> &str;
}
```

### 5.2 Flujo interno de `put`

```
put(source_path="/tmp/upload/Factura Marzo 2026.png", original_filename="Factura Marzo 2026.png")
  │
  ├─ 1. Leer archivo, calcular SHA-256
  │     sha256 = "a1b2c3d4e5f6a7b8901234567890abcdef..."
  │     hash16 = "a1b2c3d4e5f6a7b8"  (primeros 16 hex)
  │
  ├─ 2. Sanitizar nombre
  │     "Factura Marzo 2026" → "Factura_Marzo_2026"
  │     truncar a 128 chars → "Factura_Marzo_2026"
  │     extensión → "png"
  │
  ├─ 3. Construir blob_name
  │     "Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png"
  │
  ├─ 4. Crear subdirectorio staging/<prefix>/
  │     mkdir -p /var/lib/fluxbee/blob/staging/a1/
  │
  ├─ 5. Copiar archivo
  │     cp source → /var/lib/fluxbee/blob/staging/a1/Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png
  │
  └─ 6. Retornar BlobRef {
           type: "blob_ref",
           blob_name: "Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png",
           size: 5242880,
           mime: "image/png",
           filename_original: "Factura Marzo 2026.png",
           spool_day: "2026-02-24"
         }
```

### 5.3 Flujo interno de `promote`

```
promote(blob_ref)
  │
  ├─ 1. Extraer prefix desde blob_name
  │     "Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png" → prefix "a1"
  │
  ├─ 2. Construir paths
  │     from = /var/lib/fluxbee/blob/staging/a1/Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png
  │     to   = /var/lib/fluxbee/blob/active/a1/Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png
  │
  ├─ 3. Crear subdirectorio active/<prefix>/ si no existe
  │     mkdir -p /var/lib/fluxbee/blob/active/a1/
  │
  └─ 4. Rename atómico (mismo filesystem)
        rename(from, to)
```

> **Nota:** `staging/` y `active/` están en el mismo filesystem (`/var/lib/fluxbee/blob/`), por lo que `rename()` es atómico y O(1). No hay copia de datos.

### 5.4 Flujo interno de `resolve_with_retry`

```
resolve_with_retry(blob_ref, config)
  │
  ├─ 1. path = resolve(blob_ref)
  │
  ├─ 2. Si path existe → return Ok(path)
  │
  ├─ 3. Loop con backoff:
  │     delay = initial_delay_ms (100ms)
  │     total = 0
  │     while total < max_wait_ms:
  │       sleep(delay)
  │       total += delay
  │       si path existe → return Ok(path)
  │       delay = delay * backoff_factor
  │
  └─ 4. Si timeout → return Err(BLOB_NOT_FOUND)
```

---

## 6. Flujos de uso

### 6.1 Nodo IO recibe archivo externo (WhatsApp → sistema)

```
1. WhatsApp webhook entrega imagen a nodo IO
2. IO guarda temporalmente en /tmp/
3. IO llama blob.put("/tmp/image.jpg", "foto-perfil.jpg")
   → BlobRef creado, archivo en staging/
4. IO decide que el archivo está listo
5. IO llama blob.promote(blob_ref)
   → archivo pasa a active/ (atómico)
6. IO arma mensaje (contrato text/v1) con attachments = [blob_ref]
7. IO envía mensaje al router
8. Router rutea a nodo AI
9. AI recibe mensaje, lee blob_ref de attachments
10. AI llama blob.resolve(blob_ref) → path local
    (o blob.resolve_with_retry si multi-isla)
11. AI lee el archivo del path
```

### 6.2 Nodo AI genera archivo (sistema → WhatsApp)

```
1. AI comienza a generar PDF de respuesta
2. AI llama blob.put_bytes(partial_bytes, "respuesta-factura.pdf", "application/pdf")
   → BlobRef creado, archivo en staging/
   (AI puede seguir escribiendo/modificando en staging/)
3. AI decide que el archivo está completo
4. AI llama blob.promote(blob_ref)
   → archivo pasa a active/
5. AI arma mensaje (contrato text/v1) con attachments = [blob_ref]
6. AI envía mensaje al router
7. Router rutea a nodo IO
8. IO recibe mensaje, lee blob_ref
9. IO llama blob.resolve(blob_ref) → path local
10. IO lee el archivo y lo envía por WhatsApp
```

### 6.3 Nodo WF ensambla reporte con múltiples fuentes

```
1. WF recibe instrucción de generar reporte
2. WF genera sección 1, escribe a staging/ via blob.put_bytes(...)
3. WF genera sección 2, actualiza archivo en staging/
4. WF completa ensamblaje final
5. WF llama blob.promote(blob_ref) → active/
6. WF arma mensaje con attachment y envía
```

### 6.4 Mensaje sin attachments (caso común)

```
1. IO recibe texto "Hola"
2. IO arma mensaje: payload = { type: "text", content: "Hola", attachments: [] }
3. IO envía → router → AI
4. No hay blob involucrado
```

### 6.5 Flujo multi-isla (Syncthing)

```
Isla A (productor):
1. Nodo IO escribe archivo → staging/
2. IO promueve → active/
3. IO envía mensaje con BlobRef por router (canal L2)
4. Syncthing detecta nuevo archivo en active/ y comienza replicación

Isla B (consumidor):
5. Nodo AI recibe mensaje via router
6. AI llama blob.resolve_with_retry(blob_ref)
7. Si Syncthing no terminó → retry con backoff
8. Syncthing completa replicación → archivo aparece en active/ de isla B
9. AI lee el archivo
```

**Regla:** el mensaje (por router/canal L2) puede llegar antes que el archivo (por Syncthing). El consumidor maneja esto con `resolve_with_retry`.

### 6.6 Flujo recomendado v2.x (confirmación previa de propagación)

```
Isla A (productor):
1. put/put_bytes -> staging/
2. promote -> active/
3. solicitar SYSTEM_SYNC_HINT(channel=blob) a orchestrator para destinos
4. esperar confirmación (ok) o timeout explícito
5. recién entonces enviar mensaje con BlobRef

Isla B (consumidor):
6. recibe mensaje y resuelve BlobRef (resolve/resolve_with_retry)
```

**Regla v2.x recomendada:** no enviar mensaje con `BlobRef` antes de confirmación de propagación (cuando el flujo lo soporte).
`resolve_with_retry` se mantiene como fallback defensivo.

---

## 7. Promote: responsabilidad del productor

El `promote` es una decisión del nodo productor. No hay coordinación central ni protocolo de promote entre nodos.

**Regla fundamental:** un nodo solo envía un mensaje con `BlobRef` **después** de haber promovido el blob a `active/`. Nunca se referencia un blob que está en `staging/`.
En multi-isla v2.x, se recomienda además confirmar propagación antes de emitir el mensaje.

**¿Quién promueve?** Siempre el nodo que escribió el blob. El consumidor nunca promueve — solo lee de `active/`.

**¿Cuándo promover?** Cuando el nodo decide que el archivo está listo. No hay restricción de timing — puede ser inmediato (IO recibe archivo completo) o demorado (AI generando un documento largo).

**¿Hay carrera?** No. El rename es atómico: el archivo está en `staging/` o en `active/`, nunca en un estado intermedio. Si un consumidor intenta leer antes de que el archivo llegue (multi-isla), `resolve_with_retry` maneja el caso.

---

## 8. Errores canónicos

| Error | Descripción |
|-------|-------------|
| `BLOB_NOT_FOUND` | Archivo no existe en active/. En multi-isla, Syncthing puede no haber replicado aún — usar `resolve_with_retry` |
| `BLOB_IO_ERROR` | Error de lectura/escritura filesystem (disco lleno, permisos, etc.) |
| `BLOB_INVALID_NAME` | Nombre no pasa sanitización (queda vacío tras sanitizar). Fallback: usar `"blob"` |
| `BLOB_TOO_LARGE` | Archivo excede límite configurado por política (ej: 100MB) |

> **Nota:** se elimina `BLOB_HASH_MISMATCH` del draft anterior. No hay verificación de hash post-escritura porque el hash solo sirve para unicidad en el naming, no para integridad criptográfica.
> El límite es configurable vía `BlobConfig.max_blob_bytes`.

---

## 9. GC (Garbage Collection)

GC inicial implementado en `fluxbee_sdk::blob` y ejecutable desde `SY.orchestrator` (watchdog), con dos fases:

- `staging/` cleanup por TTL (`staging_ttl_hours`, default `24h`).
- `active/` GC conservador por retención (`active_retain_days`, default `30d`).

Modo operativo:

- `apply=false` (default): dry-run, solo reporta candidatos.
- `apply=true`: ejecuta borrados.

Semántica conservadora:

- En esta fase inicial, la retención de `active/` se basa en antigüedad de archivo (mtime) como aproximación de `spool_day`.
- El TTL debe mantenerse conservador para minimizar riesgo de borrar blobs aún referenciados.

**Tema pendiente futuro:** GC referencial (validar referencias vivas en storage/flujo antes de borrar definitivamente).

---

## 10. Seguridad

| Aspecto | Regla |
|---------|-------|
| Owner | `fluxbee:fluxbee` (mismo user que servicios) |
| Permisos directorio | `750` (owner rwx, group rx) |
| Permisos archivo | `640` (owner rw, group r) |
| Path traversal | `fluxbee_sdk` blob module rechaza nombres con `/`, `..`, `\` |
| Syncthing | Corre como user `fluxbee`, solo conecta a peers configurados |

---

## 11. Relación con otros componentes

| Componente | Relación con blob |
|------------|-------------------|
| **Router** | No toca blobs. Rutea mensajes con `BlobRef` como cualquier otro payload |
| **Canal de transporte** | No toca blobs. Transporta mensajes con `BlobRef` sin inspección de contenido |
| **OPA** | No toca blobs. No lee payload |
| **Syncthing** | Replica solo `active/` entre islas. `staging/` es local. Transparente |
| **Nodos (todos)** | Usan `fluxbee_sdk::blob` para put/promote/resolve |
| **SY.storage** | Persiste metadata de mensajes que contienen `BlobRef` (no el blob en sí) |

---

## 12. Constantes

```rust
// fluxbee_sdk::blob::constants

/// Máximo de caracteres del nombre fuente (antes del hash)
pub const BLOB_NAME_MAX_CHARS: usize = 128;

/// Longitud del hash truncado en el nombre (hex chars)
pub const BLOB_HASH_LEN: usize = 16;

/// Longitud del prefix de subdirectorio (chars del hash)
pub const BLOB_PREFIX_LEN: usize = 2;

/// Caracteres permitidos en nombre sanitizado
pub const BLOB_NAME_ALLOWED: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-";

/// Default timeout de resolve_with_retry (ms)
pub const BLOB_RETRY_MAX_MS: u64 = 5_000;

/// Default delay inicial de retry (ms)
pub const BLOB_RETRY_INITIAL_MS: u64 = 100;

/// Default factor de backoff
pub const BLOB_RETRY_BACKOFF: f64 = 2.0;

/// TTL de cleanup de staging/ (archivos huérfanos)
pub const BLOB_STAGING_TTL_HOURS: u64 = 24;

/// Límite default de tamaño por blob (100MB)
pub const BLOB_DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;
```

---
