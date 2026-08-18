# IO.blob / public artifacts - Specification v1

**Estado:** contrato alpha implementado y validado en Linux/Proxmox el 2026-07-17: curator,
autoridad admin, instalacion limpia de ingress, replica publica, serving `/public` y teardown estan
operativos. Resta el smoke de producto con un AI/IO real recorriendo publish/expiry/unpublish.

Leer junto con `edge-ingress-spec-v6.md` y `blob-annex-spec.md`. Esta spec define la publicacion
READ-ONLY de archivos generados dentro de Fluxbee, incluidos HTML interactivos autocontenidos, como
URLs `/public/<key>` servidas por `SY.edge`.

## 1. Decisiones cerradas

- **`SY.admin` es la autoridad.** El nodo productor llama a admin directamente. Admin conserva el
  `src_l2_name` estampado por el router, resuelve su ILK/tenant en Identity y decide si publica.
- **`IO.blob` es un worker, no una autoridad ni un relay.** Recibe un comando ya autorizado de
  `SY.admin`, cura los bytes y mantiene el ledger/refcount local. No decide tenant, acceso ni URL.
- **Ownership v1 = ownership de la publicacion.** El blob store `active/` actual es compartido y el
  `BlobRef` no contiene ownership. V1 NO afirma que puede probar criptograficamente que el publisher
  creo esos bytes; afirma que admin sabe que tenant creo y controla la publicacion.
- **Primer modo de lectura = link-capability.** No hay listing ni login en edge. Cualquiera que reciba
  el link puede verlo hasta `expires_at` o `unpublish`. La verificacion de sesion/tenant por acceso es
  la siguiente etapa, no una propiedad ficticia del primer release.
- **HTML autocontenido con JavaScript, sin red.** JS corre en el navegador, nunca en edge/IO.blob.
  Se permite interaccion local (graficos, filtros, tablas, calculos) dentro de un sandbox con
  `connect-src 'none'`. Consultar APIs o ejecutar acciones queda fuera de v1.
- **Data plane separado.** Los bytes no viajan por frames de la malla. Solo el control y metadata
  viajan por RPC; una carpeta `public/` dedicada replica bytes one-way al ingress.

## 2. Estado real del codigo al inicio

- `BlobToolkit::put_bytes/promote` escribe en `blob.path/{staging,active}` y retorna un `BlobRef`
  anonimo (`blob_name`, size, mime, filename, spool_day).
- `active/` es un store compartido, content-addressed con `hash16`; no tiene tenant/owner y hoy puede
  replicarse `sendreceive` entre hives.
- Los managed nodes reciben ILK/tenant por env, pero el router solo estampa de forma autoritativa el
  `src_l2_name` del proceso conectado.
- `SY.edge` ya sirve `/e/<ich>` por forward a un IO node. `/b/:ich` es un stub 501 y no hay static
  serving.
- El ingress se provisiona con blob/syncthing deshabilitado.

Por lo tanto, esta spec agrega un circuito nuevo y acotado; no renombra como terminado algo que ya
existia.

## 3. Invariantes

- **P1 - Producer identity survives.** El primer command mutante es `producer -> SY.admin`; no pasa
  antes por `IO.blob`. El request no acepta `tenant_id` ni `publisher_l2_name` como autoridad.
- **P2 - Admin owns policy.** Admin resuelve `src_l2_name -> ILK -> tenant`, crea `publication_id`,
  define expiracion/content policy y es la unica autoridad que puede ordenar curate/release.
- **P3 - IO.blob accepts one caller.** Solo acepta control SYSTEM desde `SY.admin@<mismo-hive>`.
- **P4 - Public folder is curated.** `IO.blob` es el unico writer normal de `blob.path/public`.
  Presence en esa carpeta no autoriza una URL.
- **P5 - DMZ never receives `active/`.** El ingress recibe solo `public/`, mediante un folder distinto
  `fluxbee-blob-public`: motherbee `sendonly`, ingress `receiveonly`.
- **P6 - Allowlist before bytes.** Edge sirve solo una `BlobRow` empujada/ACKeada por admin. Unknown,
  expired o unpublished key devuelve 404.
- **P7 - Two identifiers.** URL key = random 256-bit capability. Filename publico = SHA-256 completo
  lowercase de los bytes. La URL nunca contiene el hash del contenido.
- **P8 - Data ready before URL ready.** Admin no devuelve success hasta que edge confirma row valida,
  archivo local presente, regular y con SHA-256 esperado.
- **P9 - Dedup needs refcount.** Varias publicaciones pueden apuntar al mismo SHA. Release borra el
  archivo solo cuando no queda ninguna publicacion en el ledger.
- **P10 - Read-only.** No hay upload, PUT, DELETE ni API externa de escritura.

## 4. Flujo v1

```text
AI/IO/WF producer (tenant T)
   |  ADMIN_COMMAND publish_artifact {blob_ref, presentation, expires_in_secs?}
   v
SY.admin (authority)
   |  router-stamped src_l2 -> Identity -> tenant T
   |  policy clamp + publication_id
   |  SYSTEM BLOB_CURATE {publication_id, tenant_id, publisher_l2_name, blob_ref}
   v
IO.blob@motherbee (worker)
   |  validate BlobRef -> resolve active file -> regular/no symlink
   |  stream SHA-256 -> atomic copy/hardlink policy -> public/<sha256>
   |  durable ledger + refcount
   v
public/ (sendonly) === Syncthing ===> ingress public/ (receiveonly)
   |                                      |
   | admin retries EDGE_PUBLISH_BLOB      | edge checks file + hash
   +-------------------------------------> SY.edge
                                              |
                                              v
                                  https://content-host/public/<key>
```

### 4.1 Publish request to admin

Producer request:

```json
{
  "action": "publish_artifact",
  "params": {
    "blob_ref": {
      "type": "blob_ref",
      "blob_name": "report_ab12cd34ef56ab78.html",
      "size": 12345,
      "mime": "text/html",
      "filename_original": "report.html",
      "spool_day": "2026-07-17"
    },
    "presentation": "inline",
    "expires_in_secs": 86400
  }
}
```

Prohibido en params: `tenant_id`, `publisher_l2_name`, path, public key, sha256 filename, CSP crudo,
headers crudos o edge row. Admin deriva/minta todo eso.

**Superficie del productor AI (D1):** publicar es una **acción invocada por el modelo**, no un hook
automático — no todo HTML generado termina publicado. `ai.generic` expone la tool
`publish_html_page {filename, content}` (registrada sólo cuando el nodo tiene identidad resuelta):
valida el HTML con el mismo gate que `generate_html_artifact`, escribe el blob (`put_bytes` +
`promote`) y ejecuta `publish_artifact` contra `SY.admin` por el socket de malla, devolviendo la URL
capability al modelo para que la entregue en su respuesta. Un IO/WF productor haría lo análogo con su
propio disparador; el contrato admin es idéntico.

### 4.2 Admin -> IO.blob worker RPC

Commands SYSTEM:

- `BLOB_CURATE` -> `BLOB_CURATE_RESPONSE`
- `BLOB_RELEASE` -> `BLOB_RELEASE_RESPONSE`
- `BLOB_STATUS_GET` -> `BLOB_STATUS_GET_RESPONSE`

`BLOB_CURATE` lleva facts ya resueltos por admin. `IO.blob` valida al caller exacto pero no redecide
la autorizacion. Respuesta exitosa:

```json
{
  "status": "ok",
  "publication_id": "pub:<uuid>",
  "public_name": "<64 lowercase hex>",
  "sha256": "<64 lowercase hex>",
  "size": 12345,
  "created": true,
  "ref_count": 1
}
```

Idempotencia: repetir el mismo `publication_id` con los mismos facts devuelve success sin incrementar
refcount. Repetirlo con facts distintos devuelve conflict.

### 4.3 Admin -> edge row

`EDGE_PUBLISH_BLOB` contiene solo facts normalizados:

```json
{
  "key": "<64 hex random capability>",
  "publication_id": "pub:<uuid>",
  "public_name": "<sha256>",
  "sha256": "<sha256>",
  "size": 12345,
  "content_type": "text/html; charset=utf-8",
  "presentation": "inline",
  "expires_at": 1784400000,
  "content_policy": "sandboxed-html-v1"
}
```

Edge guarda esta row en un registry distinto del mapa ICH. El key nunca se concatena como path; el
lookup produce el `public_name` validado.

## 5. Access v1 y siguiente etapa tenant

### 5.1 V1: link-capability

`GET /public/<key>` no exige un segundo bearer. El key random es la capability. Propiedades:

- 256 bits random, exact lookup, sin listing;
- revocable por unpublish;
- `expires_at` aplicado por edge;
- `X-Robots-Tag: noindex, nofollow, noarchive`;
- `Referrer-Policy: no-referrer`;
- no prueba que el viewer pertenezca al tenant.

Cloud debe entregar el link solo al tenant correcto, pero quien lo reenvia comparte acceso. Esta es
una decision de producto explicita para el primer release.

### 5.2 Siguiente etapa: tenant-gated access

Cloud valida su sesion OAuth/tenant y solicita a admin una capability corta para una publicacion. El
edge valida firma/expiracion, sin aprender OAuth ni aceptar un tenant libre del browser. Esto agrega
un `mint_artifact_access` separado; no cambia ownership ni el data plane. Hasta implementarlo, la
spec no llama "tenant-private" a un link.

## 6. HTML interactivo autocontenido

El HTML generado por AI es codigo no confiable. Edge/IO.blob no lo ejecutan; lo ejecuta el browser.
V1 permite JavaScript local con una policy fija, no controlable por el publisher:

```text
Content-Security-Policy:
  sandbox allow-scripts;
  default-src 'none';
  script-src 'unsafe-inline';
  style-src 'unsafe-inline';
  img-src data: blob:;
  font-src data:;
  connect-src 'none';
  form-action 'none';
  object-src 'none';
  base-uri 'none'
```

Ademas:

- `X-Content-Type-Options: nosniff`;
- content host sin cookies y separado del origen Cloud/API;
- Cloud lo embebe con `iframe sandbox="allow-scripts"` cuando forma parte de su UI;
- no forms, popups, parent navigation, service workers ni requests de red;
- HTML/CSS/JS/data autocontenidos en un solo archivo para v1.

Para reporting se prefiere un renderer JS versionado y confiable que consume JSON generado por AI.
HTML+JS arbitrario sigue permitido dentro del sandbox; no obtiene APIs Fluxbee.

## 7. IO.blob worker

`IO.blob` es singleton motherbee y no externaliza ICH. Config:

- `IO_BLOB_NODE_NAME` default `IO.blob`;
- `IO_BLOB_ADMIN_HIVE` default hive local;
- `IO_BLOB_BLOB_ROOT` default `/var/lib/fluxbee/blob`;
- `IO_BLOB_PUBLIC_ROOT` default `<blob_root>/public`;
- `IO_BLOB_LEDGER_PATH` default `/var/lib/fluxbee/state/io-blob/publications.json`;
- `IO_BLOB_MAX_BYTES` default al limite canonico de Blob SDK.

Forma parte de `system_nodes.motherbee` como nodo empaquetado de lifecycle: Orchestrator inicia,
espera y supervisa `io-blob.service` junto con la topologia base. Esta inclusion no cambia su
frontera de identidad: sigue siendo un worker `IO.*` visible en el inventario de nodos administrados
y `SY.identity` no le crea una ILK deterministica de tipo `system`. `SY.vault` tambien excluye
entradas no `SY.*` al construir su allowlist local de autoridades conocidas, por lo que estar en
este lifecycle no otorga acceso al pool de secretos del tenant raiz.

Responsabilidades:

- caller gate exacto `SY.admin@<hive>`;
- validar schema/BlobRef/publication facts;
- resolver solo mediante `BlobToolkit`, nunca aceptar paths;
- rechazar symlink/no-regular y size mismatch;
- SHA-256 completo calculado por streaming;
- escritura atomica y permisos `0640`;
- ledger atomico, idempotencia y refcount;
- release seguro y status operacional;
- no Identity SHM, no tenant decision, no edge call, no public HTTP.

## 8. Edge static serving

`SY.edge` agrega un data path local acotado para `/public/:key`, reemplazando el stub `/b/`. Es una
excepcion deliberada al principio "edge solo forwardea": bytes grandes no caben en el envelope y no
deben volver a la malla.

Requisitos:

- registry tipado separado del ICH registry;
- solo `GET` y `HEAD`; `Range`/conditional GET recomendado para PDF/media;
- semaphore separado del RPC `/e/`, con permit vivo durante todo el stream;
- key -> row -> `public_name`; nunca key -> path;
- `public_name` exactamente 64 hex lowercase;
- open no-follow, regular file, canonical root y size/hash readiness al publish;
- headers derivados de policy fija, nunca headers arbitrarios del producer;
- unknown/expired/missing/unready -> 404 fail-closed.

Configuracion del edge generada por `add_ingress`:

- `edge.publications_path`: `/var/lib/fluxbee/state/sy-edge/publications.json`;
- `edge.blob_public_root`: `<blob.path>/public`;
- `edge.public_max_inflight`: default `128`;
- `edge.public_ready_timeout_ms`: default `60000`.

`GET`/`HEAD` soportan ETag y un unico byte range. El permit del semaphore vive durante todo el
stream. HTML recibe la CSP fija de la seccion 6; ningun header viene del producer.

## 9. Data replication

- Folder id nuevo: `fluxbee-blob-public`.
- Source: motherbee `<blob.path>/public`, `sendonly`.
- Destination: ingress `edge.blob_public_root`, `receiveonly`.
- Peer point-to-point; no discovery/relay/NAT traversal automatico.
- Nunca agregar ingress como device del folder `fluxbee-blob` (`active/`).
- Un publish no termina hasta que edge puede abrir/verificar el archivo.
- Unpublish: retirar row primero; release/refcount despues; delete replica en background.
- `add_ingress` copia el vendor Syncthing, ejecuta `ADD_HIVE_FINALIZE`, intercambia device IDs y
  persiste el ID del ingress para que `remove_hive` desenganche el peer.
- El perfil de peer publico es exclusivo: al enlazar un ingress no agrega ese device a
  `fluxbee-blob` ni a `fluxbee-dist`.
- El cambio aplica automaticamente a altas nuevas. Un ingress creado con el perfil anterior, sin
  `blob.sync.public_enabled`, debe reprovisionarse mediante remove/add; esta version no reescribe
  silenciosamente el `hive.yaml` de un ingress existente.

## 10. Threat boundaries honestos

- Un link capability filtrado permite lectura hasta expiry/unpublish.
- `active/` no tiene ownership fuerte. Un publisher que conoce un BlobRef puede pedir publicar esos
  bytes; admin controla quien crea la publicacion, no quien creo el contenido.
- Managed nodes corren hoy sin aislamiento Linux por tenant. Ownership fuerte de bytes requiere un
  writer mediado y workloads no-root/per-tenant; queda fuera de esta vertical.
- El DMZ contiene plaintext de los artefactos publicados. Comprometer el edge permite leer ese set,
  pero no el corpus `active/`.
- JavaScript arbitrario sigue siendo contenido hostil; sandbox + no-network es parte obligatoria del
  contrato, no una opcion del publisher.

## 11. Orden de implementacion

1. **[IMPLEMENTADO LOCAL]** `IO.blob` curator worker: RPC admin-only, full SHA-256, atomic public copy,
   ledger/refcount, unidad singleton y packaging.
2. **[IMPLEMENTADO LOCAL]** `SY.admin publish_artifact/unpublish_artifact`: direct producer gate,
   Identity tenant resolve, command a IO.blob, durable publication ledger y help mesh-only en el
   catalogo Admin (fuera del executor HTTP).
3. **[IMPLEMENTADO LOCAL]** Syncthing `fluxbee-blob-public` one-way en motherbee/ingress.
4. **[IMPLEMENTADO LOCAL]** `SY.edge /public/:key` + BlobRow + readiness/hash ACK + headers/sandbox.
5. **[VALIDADO LAB]** Infra E2E: `add_ingress` limpio, peer TCP conectado, replica one-way de HTML
   interactivo, reinicio, `GET`/`HEAD`/range/ETag/CSP y `remove_hive` sin drift de Syncthing.
6. **[IMPLEMENTADO]** Superficie productor + ciclo de vida completo (código compila, smoke en vivo
   pendiente): tool `publish_html_page` en `ai.generic` (§4.1); GC de expiración admin-side
   `run_publication_expiry_sweep` (cada 600s: `MSG_EDGE_UNPUBLISH_BLOB` + `MSG_BLOB_RELEASE`,
   ledger→`expired`, idempotente); ruta operador `POST /artifacts/unpublish` (revoca cualquier
   tenant, D2); y auditoría durable de `publish_artifact`/`unpublish_artifact` en el command-log.
7. **[PENDIENTE SMOKE PRODUCTOR]** AI/IO real genera HTML y recorre `publish_artifact`, expiry y
   `unpublish_artifact` sobre la malla instalada. Runbook: `lab/blob-public-smoke.md`.
8. **[FUTURO]** `mint_artifact_access` para validacion tenant por acceso mediante Cloud.

## 12. Fuera de v1

- Upload o escritura desde internet.
- API calls desde HTML publicado.
- Sitios multiarchivo, service workers o aplicaciones completas.
- Listing/directorio publico.
- Ownership fuerte de bytes previo a publication.
- Sesion OAuth/cookie interpretada directamente por edge.
