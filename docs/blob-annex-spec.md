# Technical Annex: Blob Storage and Attachments

**Status:** v1.1
**Date:** 2026-03-09
**Audience:** Node developers, `fluxbee_sdk` developers
**Complements:** `02-protocolo.md` §4 and §11, `07-operaciones.md`, `blob_tasks.md`

---

## 1. Principle

Every binary file (image, PDF, audio, document, generated) travels **outside the message**.
The message only carries a lightweight reference (`BlobRef`).
All nodes access the same blob repository through the local filesystem.

**There is no file transfer by message.** If a node needs to send a file to another node, it writes it to the blob storage and sends the reference. The router does not inspect or modify blobs or payload.

---

## 2. Blob Repository

### 2.1 Layout

```
/var/lib/fluxbee/blob/
├── staging/           ← node workspace: files in progress, not ready for consumption
│   └── a1/
│       └── factura_a1b2c3d4e5f6a7b8.png
├── active/            ← ready files, referenced by messages
│   └── a1/
│       └── factura_a1b2c3d4e5f6a7b8.png
└── <future: GC runs over active/ by time>
```

**`staging/`** — The nodes' working directory. Each node writes here the files it is processing, generating, downloading or transforming. A file in staging is not ready to be referenced by a message. Examples:

- **IO node:** downloading an image from WhatsApp (download in progress)
- **AI node:** generating a PDF or an image (rendering in progress)
- **WF node:** assembling a report from multiple sources
- **Any node:** converting format, resizing, processing

**`active/`** — Consolidated, referenceable files. When the producer node decides the file is ready, it promotes it from `staging/` to `active/` (atomic rename). From that moment:

- The file can be referenced in a message (`BlobRef`)
- Other nodes can read it
- Syncthing replicates it to other internal islands (if enabled)

**`staging/` is not replicated.** It is local to each island. The internal `fluxbee-blob` folder replicates
`active/` between working hives. External publication uses another folder, `fluxbee-blob-public`, which
replicates only `public/` from motherbee (`sendonly`) to ingress (`receiveonly`); it never
exposes `active/` to the DMZ.

**Prefix subdirectory** — The first 2 characters of the hash (taken from the `blob_name`). It spreads files over up to 256 subdirectories to avoid directories with thousands of entries.

### 2.2 File naming

```
<truncated_source_name>_<hash16>.<ext>
```

| Component | Rule | Example |
|------------|-------|---------|
| `source_name` | Original name of the file, sanitized and truncated to **128 chars** max | `factura-marzo-2026` |
| `_` | Fixed separator | `_` |
| `hash16` | First 16 hex characters of the SHA-256 of the content | `a1b2c3d4e5f6a7b8` |
| `.ext` | Original extension of the source file, lowercase | `.png` |

**Maximum full name:** 128 + 1 + 16 + 1 + ext(max 10) = **156 chars**

**Filesystem limits:**

| Filesystem | NAME_MAX | Our maximum | Margin |
|------------|----------|----------------|--------|
| ext4 / xfs / btrfs | 255 bytes | 156 bytes | 99 bytes |
| eCryptFS | 143 bytes | 156 bytes | ⚠️ exceeds |
| NTFS (reference) | 255 chars | 156 chars | 99 chars |

> **Note:** If the environment uses eCryptFS (uncommon on servers), reduce the source name truncation to 80 chars. The default of 128 is safe for ext4/xfs/btrfs, which are the standard production filesystems.

### 2.3 Sanitizing the source name

The producer (`fluxbee_sdk` blob module) sanitizes the original name before using it:

```
1. Extract the name without extension and the extension separately
2. Replace disallowed characters [^a-zA-Z0-9._-] → "_"
3. Collapse multiple underscores → a single one
4. Truncate the name to 128 chars (cut, do not reject)
5. Lowercase the extension
6. If the name is empty after sanitizing → use "blob"
```

**Allowed characters in a name:** `a-z A-Z 0-9 . _ -`

This guarantees compatibility with Linux, macOS, Windows (for manual inspection) and Syncthing.

---

## 3. BlobRef — Canonical structure

The `blob_name` is the unique identifier of the blob. There is no separate hash field — the hash is embedded in the name and is extracted when needed (to derive the prefix subdirectory).

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

| Field | Type | Required | Description |
|-------|------|-------------|-------------|
| `type` | string | Yes | Always `"blob_ref"` |
| `blob_name` | string | Yes | Canonical name of the file in the repository. It is the blob ID. Contains the embedded hash16 |
| `size` | integer | Yes | Size in bytes of the original file |
| `mime` | string | Yes | MIME type (`image/png`, `application/pdf`, etc.) |
| `filename_original` | string | Yes | Original name of the file as the user/system sent it. Not sanitized. For display |
| `spool_day` | string | Yes | ISO date of the day it was created. For future GC by time |

**Path resolution from a BlobRef:**

The subdirectory prefix is extracted from the `blob_name`: take the 2 characters right after the last `_` (which correspond to the first 2 hex of the hash).

```
blob_name = "factura-marzo-2026_a1b2c3d4e5f6a7b8.png"
                                  ^^
                                  prefix = "a1"

→ path = /var/lib/fluxbee/blob/active/a1/factura-marzo-2026_a1b2c3d4e5f6a7b8.png
```

**Compatibility with the existing spec:** the `blob_id` field of `02-protocolo.md` §11.2 is replaced by `blob_name`. The `blob_name` serves the same unique-identifier role.

---

## 4. Payload Contract: `text/v1`

The spec (`02-protocolo.md` §4) states that the internal structure of the payload is free and the nodes define it. This does not change.

What is defined here is the **`text/v1` contract**: the standard structure for conversational messages between nodes (IO↔AI, AI↔IO, etc.). It is a convention between nodes, not something the router imposes.

### 4.1 Structure of the `text/v1` contract

```json
{
  "payload": {
    "type": "text",
    "content": "string | empty",
    "attachments": [ BlobRef, ... ]
  }
}
```

| Field | Type | Required | Description |
|-------|------|-------------|-------------|
| `type` | string | Yes | `"text"` — identifies this contract |
| `content` | string | Yes | Message text. It can be empty (`""`) if there are only attachments |
| `attachments` | array | Yes | List of `BlobRef`. Empty array `[]` if there are no attachments |

**Rules:**

- `attachments` is always present, an empty array if there are no attachments.
- Each attachment is a full `BlobRef` pointing to a file in `active/`.
- The router does not validate this structure. It is a contract between nodes.
- Nodes that implement other flows (system commands, internal events, workflows) can use their own payload structure.

### 4.2 Examples

**Text with attachments:**

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

**Text only:**

```json
{
  "payload": {
    "type": "text",
    "content": "Hola, necesito ayuda",
    "attachments": []
  }
}
```

**Attachments only (no text):**

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

### 4.3 The 64KB rule

The 64KB limit applies to the **full JSON message** (routing + meta + serialized payload). Since blobs travel outside the message, a message with 10 attachments is still lightweight (each `BlobRef` takes ~150 bytes).

If the `content` field alone exceeds 64KB (e.g.: a huge text paste), the text content is also promoted to a blob:

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

When `content_ref` is present, `content` is omitted. The consumer reads the text from the blob. Simple rule: if it fits inline, it goes inline. If it does not fit, it goes as a `blob_ref`. The producer decides.

---

## 5. Blob Module — `fluxbee_sdk`

Blob handling is a module inside the `fluxbee_sdk` package. All nodes that need to read or write blobs use this module.

### 5.1 Public API

```rust
// fluxbee_sdk::blob

/// Blob module configuration
pub struct BlobConfig {
    pub blob_root: PathBuf,       // /var/lib/fluxbee/blob
    pub name_max_chars: usize,    // default: 128
    pub max_blob_bytes: Option<u64>, // default: Some(100MB), None => no limit
}

/// Reference to a blob in the repository (serializable to JSON)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BlobRef {
    #[serde(rename = "type")]
    pub ref_type: String,           // always "blob_ref"
    pub blob_name: String,          // canonical name (blob ID)
    pub size: u64,
    pub mime: String,
    pub filename_original: String,  // original name, not sanitized
    pub spool_day: String,          // "2026-02-24"
}

/// Statistics of a blob
pub struct BlobStat {
    pub size_on_disk: u64,
    pub exists: bool,
    pub path: PathBuf,
}

/// Retry configuration for resolve in multi-island
pub struct ResolveRetryConfig {
    pub max_wait_ms: u64,         // default: 5000
    pub initial_delay_ms: u64,    // default: 100
    pub backoff_factor: f64,      // default: 2.0
}

impl BlobToolkit {
    /// Create the toolkit with a configuration
    pub fn new(config: BlobConfig) -> Result<Self>;

    /// Write a file to the blob storage (staging/).
    /// Computes the hash, sanitizes the name, creates the subdirectory, copies the file.
    /// Returns a BlobRef. The file stays in staging/ until promote.
    pub fn put(&self, source_path: &Path, original_filename: &str) -> Result<BlobRef>;

    /// Write bytes to the blob storage (staging/).
    /// For nodes that generate content in memory.
    pub fn put_bytes(&self, data: &[u8], original_filename: &str, mime: &str) -> Result<BlobRef>;

    /// Promote a blob from staging/ to active/.
    /// Atomic rename. Call it when the file is ready for consumption.
    /// The producer decides when to promote.
    pub fn promote(&self, blob_ref: &BlobRef) -> Result<()>;

    /// Resolve the full path of a blob in active/.
    /// Returns the path immediately without checking that it exists.
    pub fn resolve(&self, blob_ref: &BlobRef) -> PathBuf;

    /// Resolve the path with retry and backoff.
    /// For multi-island environments where Syncthing may not have replicated yet.
    /// Retries up to max_wait_ms with exponential backoff.
    pub async fn resolve_with_retry(
        &self,
        blob_ref: &BlobRef,
        config: ResolveRetryConfig,
    ) -> Result<PathBuf>;

    /// Check whether a blob exists in active/.
    pub fn exists(&self, blob_ref: &BlobRef) -> bool;

    /// Get the metadata of a blob in active/.
    pub fn stat(&self, blob_ref: &BlobRef) -> Result<BlobStat>;

    /// Extract the subdirectory prefix from blob_name.
    /// Takes the 2 chars after the last '_' (first 2 hex of the hash).
    pub fn prefix(blob_name: &str) -> &str;
}
```

### 5.2 Internal flow of `put`

```
put(source_path="/tmp/upload/Factura Marzo 2026.png", original_filename="Factura Marzo 2026.png")
  │
  ├─ 1. Read the file, compute SHA-256
  │     sha256 = "a1b2c3d4e5f6a7b8901234567890abcdef..."
  │     hash16 = "a1b2c3d4e5f6a7b8"  (first 16 hex)
  │
  ├─ 2. Sanitize the name
  │     "Factura Marzo 2026" → "Factura_Marzo_2026"
  │     truncate to 128 chars → "Factura_Marzo_2026"
  │     extension → "png"
  │
  ├─ 3. Build blob_name
  │     "Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png"
  │
  ├─ 4. Open staging/<prefix>/ without following links (created 0750 if missing)
  │     /var/lib/fluxbee/blob/staging/a1/
  │
  ├─ 5. Write an exclusive temporary file (0640) and rename it over the name, by descriptor
  │     → /var/lib/fluxbee/blob/staging/a1/Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png
  │
  └─ 6. Return BlobRef {
           type: "blob_ref",
           blob_name: "Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png",
           size: 5242880,
           mime: "image/png",
           filename_original: "Factura Marzo 2026.png",
           spool_day: "2026-02-24"
         }
```

### 5.3 Internal flow of `promote`

```
promote(blob_ref)
  │
  ├─ 1. Extract the prefix from blob_name
  │     "Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png" → prefix "a1"
  │
  ├─ 2. Build the paths
  │     from = /var/lib/fluxbee/blob/staging/a1/Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png
  │     to   = /var/lib/fluxbee/blob/active/a1/Factura_Marzo_2026_a1b2c3d4e5f6a7b8.png
  │
  ├─ 3. Open active/<prefix>/ without following links (created 0750, with the owner of active/)
  │     /var/lib/fluxbee/blob/active/a1/
  │
  ├─ 4. Give the blob to the owner of active/ (fchown on the descriptor), 0640
  │
  ├─ 5. Atomic rename between the descriptors of the two folders (same filesystem)
  │     renameat(staging/a1, name, active/a1, name)
  │
  └─ 6. Check that the inode that arrived is the one that was handed over (a concurrent put can change the
        staging file between step 4 and 5); if not, also hand over the one that arrived
```

> **Reading.** A blob is read with `read_blob` (or `open_blob` to read it as a stream), never through the
> path from `resolve`: it opens the whole chain without following links, requires a regular file of the size in the
> `BlobRef` and, in `read_blob`, that the SHA-256 starts with the hash in the name (FINDINGS A-56).

> **Owners (FINDINGS A-56).** `blob/` belongs to root and the Syncthing user only traverses it;
> `staging/` belongs to root only; `active/` and `public/` belong to the Syncthing user, who serves them. The
> nodes run as root (A-60) and never act on a name in those folders that someone else could have
> planted: they open everything by descriptor and hand each blob to the owner of the folder where it ends up, so
> Syncthing can read it. Before 0.1.63 the blobs were left owned by root and a `chown -R` on each
> install covered that up.

> **Note:** `staging/` and `active/` are on the same filesystem (`/var/lib/fluxbee/blob/`), so `rename()` is atomic and O(1). There is no data copy.

### 5.4 Internal flow of `resolve_with_retry`

```
resolve_with_retry(blob_ref, config)
  │
  ├─ 1. path = resolve(blob_ref)
  │
  ├─ 2. If path exists → return Ok(path)
  │
  ├─ 3. Loop with backoff:
  │     delay = initial_delay_ms (100ms)
  │     total = 0
  │     while total < max_wait_ms:
  │       sleep(delay)
  │       total += delay
  │       if path exists → return Ok(path)
  │       delay = delay * backoff_factor
  │
  └─ 4. If timeout → return Err(BLOB_NOT_FOUND)
```

---

## 6. Usage flows

### 6.1 IO node receives an external file (WhatsApp → system)

```
1. The WhatsApp webhook delivers an image to the IO node
2. IO saves it temporarily in /tmp/
3. IO calls blob.put("/tmp/image.jpg", "foto-perfil.jpg")
   → BlobRef created, file in staging/
4. IO decides that the file is ready
5. IO calls blob.promote(blob_ref)
   → the file moves to active/ (atomic)
6. IO builds the message (text/v1 contract) with attachments = [blob_ref]
7. IO sends the message to the router
8. The router routes it to the AI node
9. AI receives the message, reads blob_ref from attachments
10. AI calls blob.resolve(blob_ref) → local path
    (or blob.resolve_with_retry if multi-island)
11. AI reads the file from the path
```

### 6.2 AI node generates a file (system → WhatsApp)

```
1. AI starts generating a response PDF
2. AI calls blob.put_bytes(partial_bytes, "respuesta-factura.pdf", "application/pdf")
   → BlobRef created, file in staging/
   (AI can keep writing/modifying in staging/)
3. AI decides that the file is complete
4. AI calls blob.promote(blob_ref)
   → the file moves to active/
5. AI builds the message (text/v1 contract) with attachments = [blob_ref]
6. AI sends the message to the router
7. The router routes it to the IO node
8. IO receives the message, reads blob_ref
9. IO calls blob.resolve(blob_ref) → local path
10. IO reads the file and sends it over WhatsApp
```

### 6.3 WF node assembles a report from multiple sources

```
1. WF receives an instruction to generate a report
2. WF generates section 1, writes to staging/ via blob.put_bytes(...)
3. WF generates section 2, updates the file in staging/
4. WF completes the final assembly
5. WF calls blob.promote(blob_ref) → active/
6. WF builds the message with the attachment and sends it
```

### 6.4 Message without attachments (common case)

```
1. IO receives the text "Hola"
2. IO builds the message: payload = { type: "text", content: "Hola", attachments: [] }
3. IO sends → router → AI
4. No blob is involved
```

### 6.5 Multi-island flow (Syncthing)

```
Island A (producer):
1. IO node writes the file → staging/
2. IO promotes → active/
3. IO sends the message with the BlobRef through the router (L2 channel)
4. Syncthing detects a new file in active/ and starts replication

Island B (consumer):
5. AI node receives the message via the router
6. AI calls blob.resolve_with_retry(blob_ref)
7. If Syncthing has not finished → retry with backoff
8. Syncthing completes replication → the file shows up in island B's active/
9. AI reads the file
```

**Rule:** the message (via router/L2 channel) can arrive before the file (via Syncthing). The consumer handles this with `resolve_with_retry`.

### 6.6 Recommended v2.x flow (prior confirmation of propagation)

```
Island A (producer):
1. put/put_bytes -> staging/
2. promote -> active/
3. request SYSTEM_SYNC_HINT(channel=blob) from the orchestrator for the destinations
4. wait for confirmation (ok) or an explicit timeout
5. only then send the message with the BlobRef

Island B (consumer):
6. receives the message and resolves the BlobRef (resolve/resolve_with_retry)
```

**Recommended v2.x rule:** do not send a message with a `BlobRef` before propagation is confirmed (when the flow supports it).
`resolve_with_retry` stays as a defensive fallback.

---

## 7. Promote: the producer's responsibility

The `promote` is a decision of the producer node. There is no central coordination nor promote protocol between nodes.

**Fundamental rule:** a node only sends a message with a `BlobRef` **after** it has promoted the blob to `active/`. A blob that is in `staging/` is never referenced.
In multi-island v2.x, it is also recommended to confirm propagation before emitting the message.

**Who promotes?** Always the node that wrote the blob. The consumer never promotes — it only reads from `active/`.

**When to promote?** When the node decides that the file is ready. There is no timing restriction — it can be immediate (IO receives a complete file) or delayed (AI generating a long document).

**Is there a race?** No. The rename is atomic: the file is in `staging/` or in `active/`, never in an intermediate state. If a consumer tries to read before the file arrives (multi-island), `resolve_with_retry` handles the case.

---

## 8. Canonical errors

| Error | Description |
|-------|-------------|
| `BLOB_NOT_FOUND` | The file does not exist in active/. In multi-island, Syncthing may not have replicated yet — use `resolve_with_retry` |
| `BLOB_IO_ERROR` | Filesystem read/write error (disk full, permissions, etc.) |
| `BLOB_INVALID_NAME` | The name does not pass sanitization (it is empty after sanitizing). Fallback: use `"blob"` |
| `BLOB_TOO_LARGE` | The file exceeds the limit configured by policy (e.g.: 100MB) |

> **Note:** `BLOB_HASH_MISMATCH` from the previous draft is removed. There is no post-write hash check because the hash only serves for uniqueness in the naming, not for cryptographic integrity.
> The limit is configurable via `BlobConfig.max_blob_bytes`.

---

## 9. GC (Garbage Collection)

Initial GC implemented in `fluxbee_sdk::blob` and runnable from `SY.orchestrator` (watchdog), with two phases:

- `staging/` cleanup by TTL (`staging_ttl_hours`, default `24h`).
- Conservative `active/` GC by retention (`active_retain_days`, default `30d`).

Operating mode:

- `apply=false` (default): dry-run, it only reports candidates.
- `apply=true`: it runs the deletions.

Conservative semantics:

- In this initial phase, the retention of `active/` is based on the file age (mtime) as an approximation of `spool_day`.
- The TTL must stay conservative to minimize the risk of deleting blobs that are still referenced.

**Pending future topic:** referential GC (check live references in storage/flow before deleting for good).

---

## 10. Security

| Aspect | Rule |
|---------|-------|
| Owner | `fluxbee:fluxbee` (same user as the services) |
| Directory permissions | `750` (owner rwx, group rx) |
| File permissions | `640` (owner rw, group r) |
| Path traversal | The `fluxbee_sdk` blob module rejects names with `/`, `..`, `\` |
| Syncthing | Runs as user `fluxbee`, only connects to configured peers |

---

## 11. Relationship with other components

| Component | Relationship with blob |
|------------|-------------------|
| **Router** | Does not touch blobs. Routes messages with a `BlobRef` like any other payload |
| **Transport channel** | Does not touch blobs. Carries messages with a `BlobRef` without inspecting the content |
| **OPA** | Does not touch blobs. Does not read the payload |
| **Syncthing** | Replicates `active/` between internal hives and, through a separate one-way folder, `public/` to ingress. `staging/` is local |
| **Nodes (all)** | Use `fluxbee_sdk::blob` for put/promote/resolve |
| **SY.storage** | Persists the metadata of messages that contain a `BlobRef` (not the blob itself) |

---

## 11.1 External publication (separate contract)

`active/` is still the shared internal spool and does not take on per-tenant ownership. Exposing an
artifact does not replicate `active/` to the DMZ nor turn a `BlobRef` into a URL. The circuit defined in
`io-blob-spec-v1.md` records the ownership of the **publication** in `SY.admin`, curates a copy by full
SHA-256 through `IO.blob` and replicates only the dedicated `public/` folder to the ingress. The URL uses a
random capability; it never uses `blob_name` or the hash as a public namespace.

---

## 12. Constants

```rust
// fluxbee_sdk::blob::constants

/// Maximum characters of the source name (before the hash)
pub const BLOB_NAME_MAX_CHARS: usize = 128;

/// Length of the truncated hash in the name (hex chars)
pub const BLOB_HASH_LEN: usize = 16;

/// Length of the subdirectory prefix (hash chars)
pub const BLOB_PREFIX_LEN: usize = 2;

/// Allowed characters in a sanitized name
pub const BLOB_NAME_ALLOWED: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-";

/// Default timeout of resolve_with_retry (ms)
pub const BLOB_RETRY_MAX_MS: u64 = 5_000;

/// Default initial retry delay (ms)
pub const BLOB_RETRY_INITIAL_MS: u64 = 100;

/// Default backoff factor
pub const BLOB_RETRY_BACKOFF: f64 = 2.0;

/// Cleanup TTL of staging/ (orphaned files)
pub const BLOB_STAGING_TTL_HOURS: u64 = 24;

/// Default size limit per blob (100MB)
pub const BLOB_DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;
```

---
