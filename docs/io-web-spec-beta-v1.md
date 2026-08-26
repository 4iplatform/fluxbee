# IO.web / Dynamic Web Applications - Beta Specification v1

**Estado:** especificacion beta de diseno. No implementada.

**Fecha:** 2026-07-18.

**Audiencia:** desarrollo de `IO.web`, `SY.edge`, `SY.admin`, Orchestrator, Fluxbee Cloud y runtime/ops.

**Relacionadas:** `edge-ingress-spec-v6.md`, `io-blob-spec-v1.md`, `io-cloud-spec-v1.md`,
`blob-annex-spec.md`, `node-spawn-config-spec.md`, `runtime-lifecycle-spec.md` y
`system-inventory-spec.md`.

Esta spec define como publicar aplicaciones web dinamicas creadas dentro de Fluxbee. Una aplicacion
puede servir HTML/CSS/JavaScript, exponer una API y acceder a fuentes de datos internas sin exponer
la base de datos, los repositorios ni el control plane al ingress.

La vertical es deliberadamente distinta de la publicacion de artefactos de `IO.blob`:

- `IO.blob` publica bytes read-only y autocontenidos en `/public/<key>`;
- `IO.web` despliega una aplicacion con lifecycle, origin HTTP, sesiones y operaciones de datos;
- `SY.edge` no ejecuta la aplicacion: termina TLS, resuelve una ruta autorizada y hace reverse proxy
  hacia un unico gateway interno `IO.web` mediante mTLS;
- `SY.admin` conserva la autoridad sobre tenant, deployment, bindings de datos y apertura/cierre de
  la ruta publica.

---

## 1. Decisiones beta cerradas

1. **Tres superficies, tres registros.** `SY.edge` mantiene separados:
   - `/e/<ich>` para HTTP convertido a mensajes Fluxbee;
   - `/public/<key>` para artefactos read-only de un solo archivo;
   - aplicaciones `IO.web` para reverse proxy HTTP real.
2. **`app_key` selecciona una aplicacion; no identifica al usuario.** Para aplicaciones privadas o
   mutantes se requiere una sesion separada. Conocer la URL no prueba tenant ni permisos.
3. **Origin por aplicacion.** La URL canonica usa un subdominio aleatorio, no un path compartido:
   `https://<app_key>.apps.<dominio>/`.
4. **Un unico upstream permitido en Edge.** Una route row nunca contiene host, IP, URL o puerto
   elegidos por el publisher. Edge solo puede conectar al perfil de infraestructura `io-web-v1`.
5. **mTLS en el data plane interno.** Edge e `IO.web` se autentican mutuamente. El listener de
   `IO.web` no es publico y el firewall acepta solamente ingress hives autorizados.
6. **Admin es autoridad.** El productor pide el deployment directamente a Admin para conservar el
   `src_l2_name` estampado por el router. `IO.web` y Edge reciben facts ya autorizados.
7. **Runtime beta declarativo.** `IO.web` sirve bundles web y ejecuta operaciones de datos acotadas.
   No evalua JavaScript server-side, shell, Python, PHP ni binarios generados por AI.
8. **Credenciales nunca cruzan Edge.** DB/repository credentials viven dentro de la frontera y se
   resuelven por referencias administradas. No aparecen en bundle, manifest, browser ni route row.
9. **Deployment inmutable y switch atomico.** Una version se materializa y valida completa antes de
   que Edge la haga visible. Actualizar crea un nuevo `deployment_id`; no modifica archivos activos.
10. **El proxy no reemplaza la malla.** Control, autoridad, inventory y lifecycle siguen usando
    mensajes Fluxbee. HTTP directo se usa solo para el data plane web que necesita streaming.

---

## 2. Objetivos y no objetivos

### 2.1 Objetivos beta

- Permitir que un nodo `AI.*`, `IO.*` o `WF.*` publique una aplicacion web ya generada.
- Servir sitios multiarchivo con HTML, CSS, JS, imagenes y fuentes locales.
- Dar a cada aplicacion un origin independiente y una URL revocable.
- Exponer APIs same-origin bajo `/api/*` sin publicar puertos adicionales.
- Conectar operaciones API a DB o repositorios internos mediante bindings autorizados.
- Mantener una unica puerta publica HTTPS en `SY.edge`.
- Conservar tenant y publisher desde la identidad estampada, no desde payload libre.
- Soportar stage, health, activate, switch, rollback, drain y undeploy.
- Aislar limites de concurrencia, body, timeout y conexiones por aplicacion.
- Mantener el ingress descartable y reconstruible desde autoridades internas.

### 2.2 Fuera de beta

- Ejecutar backend arbitrario generado por AI.
- Dar shell, compilador o package manager al runtime web.
- Conectar el browser directamente a PostgreSQL, filesystem, Git o Vault.
- Aceptar SQL crudo, paths locales, URLs internas o credentials desde requests publicos.
- Upstreams arbitrarios configurados por tenant o publisher.
- Custom domains por aplicacion.
- WebSockets, HTTP CONNECT, TRACE, gRPC y upgrades de protocolo.
- Uploads grandes o media streaming de larga duracion.
- Service workers y aplicaciones offline.
- Egress libre a internet desde `IO.web`.
- Aislamiento fuerte para codigo server-side no confiable. Eso requiere otra fase y otro runtime.
- Convertir Edge en un API gateway de control plane.

---

## 3. Terminologia e identificadores

| Termino | Forma | Autoridad | Semantica |
|---|---|---|---|
| `app_id` | `app:<uuid>` | Admin | Identidad durable de la aplicacion logica |
| `deployment_id` | `dep:<uuid>` | Admin | Version inmutable y desplegable de una app |
| `app_key` | 52 chars base32 lowercase | Admin | Label DNS aleatorio de 256 bits; selecciona route/origin |
| `release_hash` | 64 hex lowercase | `IO.web` | SHA-256 canonico del manifest y todos los archivos |
| `binding_id` | `bind:<uuid>` | Admin | Binding autorizado entre app y recurso de datos |
| `config_ref` | Vault key name | Admin | Referencia interna del binding; nunca contiene el secret value |
| `access_code` | opaco, one-time | Admin | Bootstrap corto de sesion, emitido bajo autoridad Cloud |
| `session_id` | opaco | `IO.web` | Sesion de browser scopeada a app/tenant/subject |

### 3.1 Por que `app_key` no usa 64 hex

Un label DNS tiene maximo 63 caracteres. Un token de 32 bytes en hex requiere 64. Para conservar
256 bits dentro de un label se usa base32 lowercase sin padding: 52 caracteres `[a-z2-7]{52}`.

### 3.2 URL canonica

```text
https://<app_key>.apps.<public_domain>/
```

Ejemplo no real:

```text
https://mfrggzdfmztwq2lk...52chars.apps.example.net/
```

Requisitos:

- DNS wildcard `*.apps.<public_domain>` apunta al ingress.
- El certificado cubre exactamente ese wildcard.
- `app_key` se compara por lookup exacto; no hay listing ni prefijos.
- Un key desconocido, revocado o expirado responde 404.
- Rotar `app_key` crea una route nueva y revoca la anterior.

Un fallback path-based `/apps/<app_key>/` puede existir solo para laboratorio. No es canonico para
produccion porque los paths no crean origins distintos.

### 3.3 Por que la URL no usa `release_hash`

El "hash del link" queda separado en dos conceptos. `release_hash` identifica contenido de forma
deterministica: cambia al actualizar archivos y puede repetirse si dos deployments tienen el mismo
bundle. Por eso no representa ownership, acceso, lifecycle ni una aplicacion durable y no se expone
como selector de route.

`app_key` es un capability locator aleatorio y revocable que selecciona `app_id`; puede sobrevivir a
un update y rotarse sin reconstruir el release. Edge busca `app_key` y luego fija el
`deployment_id/release_hash` activo. Ninguno de los dos autentica al usuario.

---

## 4. Arquitectura

```text
                                 CONTROL PLANE (Fluxbee messages)

AI/IO/WF producer
      | ADMIN_COMMAND deploy_web_app {bundle_ref, app_id?, access_mode, bindings[]}
      v
SY.admin (authority)
      | resolve router-stamped publisher -> ILK -> tenant
      | mint app_id/deployment_id/app_key; validate grants and bindings
      | WEB_STAGE
      v
IO.web@motherbee
      | materialize immutable release; validate manifest/files/routes
      | initialize connector pools; local health probe
      | WEB_STAGE_RESPONSE {ready, release_hash}
      v
SY.admin
      | EDGE_OPEN_WEB_APP / EDGE_SWITCH_WEB_APP
      v
SY.edge@ingress (rebuildable route cache)
      | ACK route active
      v
SY.admin -> producer {url, app_id, deployment_id, status=active}


                                  DATA PLANE (HTTP streaming)

Browser
      | HTTPS :443, Host=<app_key>.apps.<domain>
      v
SY.edge@ingress
      | exact route lookup + limits + header sanitation
      | HTTPS/mTLS to fixed upstream profile io-web-v1
      v
IO.web private listener :8443
      | app_key -> active deployment
      | / and static assets -> immutable release
      | /api/* -> declarative operation -> approved binding
      v
PostgreSQL / repository / internal data service
```

### 4.1 Planos separados

| Plano | Transporte | Autoridad | Datos permitidos |
|---|---|---|---|
| Deployment/control | Router/WAN Fluxbee | Admin | IDs, facts, status, hashes |
| Web data | HTTPS/mTLS directo | Edge + `IO.web` | HTTP request/response streaming |
| Identity | Identity SHM/RPC | `SY.identity` | publisher, subject, tenant |
| Secrets | Vault RPC interno | `SY.vault` | refs y values nunca publicos |
| Bundle ingestion | BlobRef interno | producer + `IO.web` | archivo de release, no URL publica |

El uso de un `BlobRef` como transporte del bundle no convierte la aplicacion en una publicacion
`IO.blob`. `IO.blob` no participa, no copia el bundle al DMZ y no crea `/public/<key>`. Edge recibe
requests dinamicos y `IO.web` sirve la version activa desde el hive interno.

---

## 5. Autoridad y fronteras de confianza

### 5.1 Productor

Un managed node genera el bundle, lo promueve mediante Blob SDK y llama directamente a Admin. El
request no puede declarar como autoridad:

- `tenant_id`;
- `publisher_l2_name` o `publisher_ilk`;
- `app_key`;
- `deployment_id`;
- edge target;
- upstream host/IP/port;
- Vault key o credential;
- headers publicos o CSP cruda.

Admin deriva publisher y tenant desde el `src_l2_name` router-stamped y Identity. Un caller sin
identidad completa o tenant activo falla cerrado.

### 5.2 Admin

Admin es autoridad durable sobre:

- owner de `app_id`;
- tenant de la app;
- deployments y su estado;
- bindings de datos concedidos;
- access mode;
- route publica activa;
- expiracion y revocacion;
- historial de switch/rollback.

Admin no sirve bytes ni requests web. Su ledger debe permitir reconstruir Edge e `IO.web` despues
de reinicios.

### 5.3 IO.web

`IO.web` es gateway y runtime confiable, no autoridad de tenant. Acepta control mutante solamente de
`SY.admin@<same-hive>`. Verifica consistencia e integridad, pero no reemplaza las decisiones de
ownership de Admin.

En el data plane, `IO.web` autoriza sesiones y operaciones contra los facts de deployment que Admin
instalo. Nunca toma `tenant_id`, `binding_id` o deployment desde headers externos sin validar.

### 5.4 Edge

Edge es frontera publica y cache reconstruible. Puede decidir:

- si el host corresponde a una route activa;
- si el metodo/tamano/capacidad se permite;
- si el upstream mTLS es autentico;
- si el request se puede forwardear.

Edge no decide ownership, no resuelve Identity, no conoce DB y no recibe secrets de conectores.

### 5.5 App key versus acceso

`app_key` evita enumeracion y selecciona origin, pero no es suficiente para operaciones sobre datos
privados. Los access modes beta son:

- `public`: cualquier viewer con URL puede abrir la app. Solo operaciones explicitamente anonimas
  y read-only pueden ejecutarse.
- `cloud-session`: requiere sesion corta scopeada a `{app_id, tenant_id, subject, exp}`. Es obligatorio
  para datos privados y toda mutacion.

### 5.6 Precondicion de identidad para Vault

El modelo owner-scoped existe, pero el codigo actual de Vault autoriza el owner comparando contra
`meta.src_ilk`. La auditoria de `IO.cloud` deja registrado que ese campo aun no esta demostrado como
derivado de la conexion autenticada: el router estampa `src_l2_name`, pero solo canonicaliza el ILK
aseverado por el caller.

Antes de habilitar bindings o sesiones cuyos secrets dependan de esa frontera, Router/Vault deben
atar el ILK efectivo al `src_l2_name` autoritativo mediante Identity, o Vault debe derivar el caller
desde ese nombre y rechazar todo mismatch. Debe existir una prueba adversarial donde `IO.web`
asevera el ILK de otro nodo y recibe `UNAUTHORIZED`. Hasta cerrar esto, Fase 1 puede servir static
sin bindings, pero Fases 2-4 no cumplen el modelo de seguridad de esta spec.

---

## 6. IO.web como nodo Fluxbee

### 6.1 Rol y lifecycle

Beta define `IO.web@motherbee` como runtime managed motherbee-only (boot=true), similar operacionalmente a `IO.blob` e
`IO.cloud`, pero con un listener privado adicional.

- binario: `/usr/bin/io-web`;
- unit: `io-web.service`;
- role gate: `motherbee`;
- node name default: `IO.web`;
- lifecycle declarado en `system_nodes.motherbee.nodes` y `wait_for`;
- Orchestrator inicia, espera y supervisa el service;
- aparece en inventory como `IO.*`;
- no recibe ILK deterministica `system` por estar en `system_nodes`;
- debe self-provisionar una ILK `agent` como hace `IO.cloud`, o recibir una ILK provisionada por el
  bootstrap antes de usar Vault.

La allowlist exacta de packaged non-system nodes de Orchestrator/Identity debe extenderse para
`IO.web`. Vault ya debe continuar filtrando solo entradas `SY.*` al construir autoridades system.

### 6.2 Configuracion propuesta

```yaml
web:
  enabled: true
  listen: "192.168.103.10:8443"
  public_domain: "apps.example.net"
  state_dir: "/var/lib/fluxbee/state/io-web"
  releases_dir: "/var/lib/fluxbee/web/releases"
  staging_dir: "/var/lib/fluxbee/web/staging"
  tls:
    cert_path: "/etc/fluxbee/tls/io-web/server.crt"
    key_path: "/etc/fluxbee/tls/io-web/server.key"
    client_ca_path: "/etc/fluxbee/tls/io-web/ingress-ca.crt"
  limits:
    max_apps: 1000
    max_bundle_bytes: 67108864
    max_files_per_release: 4096
    max_unpacked_bytes: 268435456
    max_path_depth: 16
    max_request_body_bytes: 1048576
    max_api_response_bytes: 2097152
```

Overrides equivalentes pueden existir como `IO_WEB_*`, pero `hive.yaml` es la fuente declarativa.

### 6.3 Estado local

```text
/var/lib/fluxbee/state/io-web/
  deployments.json
  bindings.json
  sessions.db
  audit/

/var/lib/fluxbee/web/
  staging/<deployment_id>-<random>/
  releases/<release_hash>/
```

Reglas:

- `releases/<release_hash>` es inmutable.
- No se usan symlinks para seleccionar la version activa; el ledger apunta al hash.
- Los ledgers se escriben temp + fsync + atomic rename.
- Staging incompleto nunca es visible al HTTP server.
- Startup valida schema, rutas, hashes y ausencia de symlinks antes de marcar ready.
- Sesiones y tokens no se guardan en JSON de deployment.

---

## 7. Bundle y manifest de aplicacion

### 7.1 Formato de ingreso

El productor entrega un `BlobRef` con MIME `application/vnd.fluxbee.web+zip`. El ZIP contiene un
archivo obligatorio `fluxbee.web.json` en root y los assets de frontend.

`IO.web` extrae mediante una libreria ZIP probada. No se implementa parsing manual.

Validaciones obligatorias antes de escribir cada entry:

- path UTF-8 valido y relativo;
- rechazo de path absoluto, drive prefix, NUL, backslash y segmentos `.`/`..`;
- rechazo de symlink, hardlink, device, FIFO y entry no regular;
- rechazo de nombres duplicados y colisiones case-folding;
- limites de cantidad, profundidad, bytes por archivo y total expandido;
- limite de compression ratio para evitar zip bombs;
- un solo `fluxbee.web.json` y un solo entrypoint;
- no dotfiles salvo allowlist del formato;
- hash SHA-256 completo de cada archivo durante extraction;
- permisos finales read-only para el usuario de servicio;
- staging y releases en el mismo filesystem para atomic rename.

### 7.2 Manifest beta

Ejemplo:

```json
{
  "schema_version": 1,
  "app": {
    "name": "sales-reporting",
    "entrypoint": "index.html",
    "spa_fallback": true,
    "security_profile": "web-app-v1"
  },
  "assets": {
    "cache_mode": "content-addressed"
  },
  "api": {
    "routes": [
      {
        "method": "GET",
        "path": "/api/reports/:report_id",
        "operation": "reports.get"
      },
      {
        "method": "POST",
        "path": "/api/reports/:report_id/notes",
        "operation": "reports.add_note"
      }
    ],
    "operations": {
      "reports.get": {
        "binding": "analytics",
        "action": "report.read.v1",
        "anonymous": false,
        "params_schema": {
          "type": "object",
          "required": ["report_id"],
          "properties": {
            "report_id": {"type": "string", "maxLength": 64}
          },
          "additionalProperties": false
        }
      },
      "reports.add_note": {
        "binding": "analytics",
        "action": "report.note.create.v1",
        "anonymous": false,
        "csrf": true,
        "params_schema": {
          "type": "object",
          "required": ["report_id", "text"],
          "properties": {
            "report_id": {"type": "string", "maxLength": 64},
            "text": {"type": "string", "maxLength": 2000}
          },
          "additionalProperties": false
        }
      }
    }
  }
}
```

El manifest referencia bindings por alias logico y acciones por ID. No contiene:

- connection strings;
- Vault refs elegidas por el producer;
- SQL;
- filesystem roots;
- upstream URLs;
- shell commands;
- response headers crudos;
- CSP cruda;
- tenant o subject claims.

### 7.3 Release hash

`IO.web` genera un manifest interno canonico con `{path, sha256, size, content_type}` ordenado por
path. `release_hash` es SHA-256 de esa representacion canonica mas el manifest funcional validado.

El hash permite:

- deduplicar releases identicos;
- verificar integridad en startup;
- cachear assets inmutables;
- demostrar que el deployment activo no cambio;
- rollback sin reextraer.

---

## 8. Contratos de Admin

Los nombres siguientes son propuestos y no existen todavia en el action registry.

### 8.1 `deploy_web_app`

Operacion mesh-only iniciada directamente por el producer.

Request permitido:

```json
{
  "action": "deploy_web_app",
  "params": {
    "bundle_ref": {
      "type": "blob_ref",
      "blob_name": "sales_web_ab12cd34ef56ab78.zip",
      "size": 420000,
      "mime": "application/vnd.fluxbee.web+zip",
      "filename_original": "sales-web.zip",
      "spool_day": "2026-07-18"
    },
    "app_id": null,
    "access_mode": "cloud-session",
    "binding_aliases": ["analytics"],
    "expires_in_secs": 604800,
    "request_id": "req:<uuid>"
  }
}
```

Response:

```json
{
  "status": "ok",
  "app_id": "app:<uuid>",
  "deployment_id": "dep:<uuid>",
  "release_hash": "<sha256>",
  "app_key": "<52-char-base32>",
  "url": "https://<app_key>.apps.example.net/",
  "access_mode": "cloud-session",
  "state": "active"
}
```

Admin debe:

1. autorizar caller family y obtener `src_l2_name` estampado;
2. resolver ILK, tenant y estado de registro;
3. verificar que un `app_id` existente pertenece al mismo tenant/publisher o que existe grant;
4. resolver `binding_aliases` contra grants ya existentes del tenant;
5. clamp de expiracion, limites y security profile;
6. mint de IDs y key;
7. ejecutar stage en `IO.web`;
8. abrir/switchear Edge solo despues de readiness;
9. persistir estado active despues del ACK;
10. devolver URL solo cuando toda la transaccion esta activa.

### 8.2 `update_web_app`

Requiere `app_id`, `bundle_ref`, `request_id` y opcionalmente cambios compatibles de bindings/access.
Siempre crea un nuevo `deployment_id`. No modifica el release activo.

Flujo:

1. stage new;
2. validate + health;
3. atomic edge switch al nuevo deployment;
4. mark old `draining`;
5. esperar drain timeout;
6. retener old para rollback segun policy;
7. release posterior.

### 8.3 `undeploy_web_app`

Requiere `app_id` o `deployment_id`. El caller normal solo puede revocar apps propias. Admin:

1. cierra route en Edge y espera ACK;
2. invalida sesiones;
3. marca deployment revoked;
4. drena requests en `IO.web`;
5. libera pools/bindings;
6. borra release solo cuando refcount y retention lo permiten.

### 8.4 Lecturas

- `get_web_app_status {app_id}`;
- `list_web_apps {owned_only:true, cursor?, limit?}`;
- `get_web_deployment {deployment_id}`;
- `get_web_app_audit {app_id, cursor?, limit?}`.

Las respuestas grandes se paginan. No viajan bodies, bundles, credentials ni logs crudos por frames.

### 8.5 `mint_web_access`

Operacion destinada a Fluxbee Cloud, no a cualquier producer. El bearer de servicio de Cloud
autoriza a `IO.cloud` a solicitar un access bootstrap para un usuario/tenant ya autenticado por
Cloud.

Semantica minima del token firmado u opaco:

```json
{
  "app_id": "app:<uuid>",
  "tenant_id": "tnt:<uuid>",
  "subject": "cloud-user:<opaque>",
  "aud": "io.web",
  "iat": 1784300000,
  "exp": 1784300120,
  "jti": "<random>",
  "one_time": true
}
```

Reglas:

- TTL bootstrap maximo: 120 segundos.
- `jti` se consume una sola vez.
- El service bearer de Cloud nunca llega al browser ni a `IO.web`.
- Cloud puede asegurar subject/tenant porque su service token es la autoridad alpha actual.
- Admin verifica que app y tenant coinciden antes de mint.
- Formato criptografico y rotacion de signing keys deben congelarse antes de implementar. La
  semantica de claims de esta seccion es normativa aunque el encoding no lo sea todavia.

---

## 9. Protocolos SYSTEM propuestos

### 9.1 Admin -> IO.web

- `WEB_STAGE` -> `WEB_STAGE_RESPONSE`
- `WEB_ACTIVATE` -> `WEB_ACTIVATE_RESPONSE`
- `WEB_DRAIN` -> `WEB_DRAIN_RESPONSE`
- `WEB_RELEASE` -> `WEB_RELEASE_RESPONSE`
- `WEB_STATUS_GET` -> `WEB_STATUS_GET_RESPONSE`
- `WEB_RECONCILE` -> `WEB_RECONCILE_RESPONSE`

Los verbs mutantes se agregan al SDK y a `PROTECTED_SYSTEM_ACTIONS`/`policy/system.rego`. Ademas,
`IO.web` aplica un gate de handler exacto para `SY.admin@motherbee`; no basta con que un mensaje sea
SYSTEM ni con compartir VPN. Las responses deben conservar trace y destination del request.

Ejemplo `WEB_STAGE`:

```json
{
  "app_id": "app:<uuid>",
  "deployment_id": "dep:<uuid>",
  "app_key": "<52-char-base32>",
  "tenant_id": "tnt:<uuid>",
  "publisher_l2_name": "AI.builder@motherbee",
  "bundle_ref": {"type": "blob_ref", "blob_name": "...", "size": 420000},
  "access_mode": "cloud-session",
  "bindings": [
    {
      "alias": "analytics",
      "binding_id": "bind:<uuid>",
      "connector_type": "postgres",
      "resource_profile": "reporting-readwrite-v1",
      "config_ref": "web/bindings/bind:<uuid>/config"
    }
  ],
  "limits": {
    "max_request_body_bytes": 1048576,
    "max_api_response_bytes": 2097152,
    "max_concurrency": 64
  }
}
```

`config_ref` es solo el nombre estable de una entrada de Vault. Admin lo toma del binding durable;
el producer no puede enviarlo, reemplazarlo ni inferirlo desde un alias. El secret se crea despues de
registrar la identidad de `IO.web`, con `metadata.owner_node="IO.web@motherbee"`, para que Admin
resuelva su ILK canonico y Vault autorice la lectura owner-scoped existente. El payload SYSTEM puede
contener binding IDs, perfiles y esa referencia ya autorizados, pero nunca secret values.

Al hacer `WEB_STAGE`, `IO.web` usa `VaultClient` con su propia identidad para leer `config_ref` y
valida la metadata y el contenido tipado contra `binding_id`, `tenant_id`, `connector_type` y
`resource_profile`. Una referencia ausente, una lectura no autorizada, un tipo incorrecto o una
inconsistencia dejan el deployment en `failed`; no existe fallback a variables globales, tenant
pool, otra key o configuracion del bundle.

### 9.2 Admin -> Edge

- `EDGE_OPEN_WEB_APP` -> `EDGE_OPEN_WEB_APP_RESPONSE`
- `EDGE_SWITCH_WEB_APP` -> `EDGE_SWITCH_WEB_APP_RESPONSE`
- `EDGE_CLOSE_WEB_APP` -> `EDGE_CLOSE_WEB_APP_RESPONSE`
- `EDGE_LIST_WEB_APPS` -> `EDGE_LIST_WEB_APPS_RESPONSE`
- `EDGE_RECONCILE_WEB_APPS` -> `EDGE_RECONCILE_WEB_APPS_RESPONSE`

Solo `SY.admin@motherbee` puede mutar este registry, conservando la autoridad de Edge actual.
Orchestrator, Internet e `IO.web` no llaman estos mensajes.

Estos verbs deben extender en conjunto los gates exactos actuales del router para Edge:

- `PROTECTED_SYSTEM_ACTIONS`, la tabla Rust y `policy/system.rego`;
- `is_edge_service_action`;
- `is_edge_service_control_message` y `edge_service_control_allowed`;
- `edge_service_control_response_allowed`;
- los tests que exigen origen `SY.admin@motherbee` y niegan otros hives/roles.

Agregar solo el handler en `SY.edge` no funciona: el router actual falla cerrado para SYSTEM traffic
desconocido que toca Edge.

### 9.3 Idempotencia

- Repetir mismo `request_id` y mismos facts devuelve el resultado anterior.
- Reutilizar ID con facts distintos devuelve conflict.
- `EDGE_OPEN_WEB_APP` con row identica es success.
- `EDGE_SWITCH_WEB_APP` requiere `expected_deployment_id` para compare-and-swap.
- `WEB_RELEASE` de un deployment ya liberado es success con `released:false`.

---

## 10. Cambios requeridos en SY.edge

### 10.1 Compatibilidad

No se modifican los contratos existentes:

- `/e/:ich` y `/e/:ich/*extra` siguen HTTP -> Fluxbee message;
- `/public/:key` sigue static single-file con registry/hash/readiness;
- `/healthz` sigue operacional;
- el nuevo proxy usa un registry, semaphores y metricas propios.

El path `/e` actual no sirve como proxy web generico: bufferiza body dentro de un envelope de 64 KiB,
filtra headers y convierte la response del nodo en JSON. Extender ese envelope para servir sitios
romperia el limite de frame y mezclaria el data plane con la malla.

### 10.2 Configuracion de upstream

```yaml
edge:
  web_apps:
    enabled: true
    public_domain: "apps.example.net"
    routes_path: "/var/lib/fluxbee/state/sy-edge/web-apps.json"
    upstream_profile: "io-web-v1"
    upstream_url: "https://192.168.103.10:8443"
    upstream_server_name: "io-web.motherbee.fluxbee.internal"
    client_cert_path: "/etc/fluxbee/tls/io-web/client.crt"
    client_key_path: "/etc/fluxbee/tls/io-web/client.key"
    ca_path: "/etc/fluxbee/tls/io-web/ca.crt"
    connect_timeout_ms: 2000
    first_byte_timeout_ms: 15000
    request_timeout_ms: 30000
    idle_stream_timeout_ms: 30000
    max_inflight: 1024
    max_inflight_per_app: 64
    max_request_body_bytes: 1048576
    max_response_bytes: 33554432
```

`upstream_url` es infraestructura escrita por `add_ingress`, no parte de una route row ni de un
command de producer.

### 10.3 Route row

```json
{
  "app_key": "<52-char-base32>",
  "app_id": "app:<uuid>",
  "deployment_id": "dep:<uuid>",
  "release_hash": "<sha256>",
  "upstream_profile": "io-web-v1",
  "access_mode": "cloud-session",
  "methods": ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"],
  "expires_at": 1784900000,
  "max_request_body_bytes": 1048576,
  "max_inflight": 64
}
```

Prohibido en row:

- tenant o subject;
- upstream URL/host/IP/port;
- credentials;
- raw request/response headers;
- raw CSP;
- filesystem paths;
- DB/repository references.

### 10.4 Host routing

Para cada request Edge debe:

1. requerir un unico Host/`:authority` valido;
2. remover puerto y normalizar ASCII lowercase;
3. rechazar userinfo, whitespace, control chars, trailing ambiguity y absolute-form inconsistente;
4. verificar suffix exacto `.apps.<public_domain>`;
5. extraer un solo label anterior al suffix;
6. validar `[a-z2-7]{52}`;
7. hacer lookup exacto en el registry;
8. devolver 404 uniforme si key no existe, expiro o fue revocado.

No hay wildcard de route, substring matching ni filesystem lookup basado en Host.

### 10.5 Request proxying

Edge usa una libreria HTTP probada sobre Hyper, con streaming y connection pooling. No implementa
parsing HTTP, chunking ni framing manualmente.

Normas:

- external HTTP/1.1 y HTTP/2 segun el server actual;
- upstream HTTP/1.1 sobre TLS beta;
- body streaming con contador y hard limit;
- `CONNECT`, `TRACE` y cualquier Upgrade se rechazan;
- no retry automatico de requests mutantes;
- beta no reintenta ni GET despues de enviar bytes al upstream;
- query y path se preservan sin double decode;
- no se normalizan segmentos de forma que dos paths distintos colisionen;
- request ID se genera o valida con limite y se reemplaza como valor trusted.

Headers inbound removidos siempre:

- hop-by-hop (`Connection`, `Keep-Alive`, `Proxy-*`, `TE`, `Trailer`, `Transfer-Encoding`, `Upgrade`);
- `Forwarded` y todos los `X-Forwarded-*`;
- todos los `X-Fluxbee-*`;
- `Host` original al hablar con upstream;
- service bearer de Cloud;
- cualquier header de identidad/tenant inyectable por browser.

Edge inyecta solo sobre el canal mTLS:

```text
X-Fluxbee-App-Id: app:<uuid>
X-Fluxbee-Deployment-Id: dep:<uuid>
X-Fluxbee-App-Key: <app_key>
X-Fluxbee-Request-Id: <uuid>
X-Forwarded-Proto: https
X-Forwarded-Host: <canonical-app-host>
```

`IO.web` confia en esos headers solamente en el listener mTLS y reemplaza/rechaza duplicados.

### 10.6 Response proxying

- Preservar status HTTP real.
- Stream de body; no serializar en JSON ni frames Fluxbee.
- Remover hop-by-hop, `Server`, `X-Powered-By` y headers de infraestructura.
- Aplicar `X-Content-Type-Options: nosniff`, HSTS y `Referrer-Policy` desde Edge.
- CSP se selecciona por security profile validado; la app no envia CSP cruda.
- `Location` externo se rechaza o se permite solo por policy explicita.
- `Set-Cookie` solo se permite para cookies emitidas por el session endpoint de `IO.web` y que
  cumplan la seccion 12.
- CORS no se refleja desde request. Default: no CORS.
- Respuestas que excedan el limite se cortan y registran como policy violation.

### 10.7 Backpressure y fallas

Edge mantiene limites globales y por app durante toda la vida del stream.

| Condicion | HTTP |
|---|---:|
| host/route desconocida, revocada o expirada | 404 |
| sesion faltante (respuesta de IO.web) | 401 |
| operacion no autorizada | 403 |
| metodo no permitido | 405 |
| body demasiado grande | 413 |
| capacidad por app agotada | 429 |
| capacidad global agotada | 503 |
| mTLS/upstream no disponible | 502/503 |
| timeout upstream | 504 |

Los errores no revelan IP, puerto, deployment interno, tenant, stack trace ni causa de TLS.

### 10.8 Cache y reconstruccion

El registry local se persiste atomicamente para warm start, pero no es autoridad. Al conectar Admin:

- Edge reporta fingerprint/version;
- Admin compara con desired state;
- aplica snapshot/delta idempotente;
- routes extra se eliminan;
- secrets no se persisten en rows.

---

## 11. Data plane dentro de IO.web

### 11.1 Resolucion de deployment

`IO.web` recibe `X-Fluxbee-App-Key`, `App-Id` y `Deployment-Id` desde Edge y verifica que los tres
coinciden con su ledger active. Un mismatch devuelve 404/409 y genera alerta; no intenta adivinar.

### 11.2 Routing interno reservado

Precedencia:

1. `/_fluxbee/session/*`: bootstrap/logout/session status;
2. `/api/*`: rutas declaradas en manifest;
3. static exact path;
4. SPA fallback opcional solo para GET/HEAD que aceptan HTML;
5. 404.

La app no puede declarar rutas bajo `/_fluxbee/`. No hay directory listing.

### 11.3 Static serving

- `/` sirve entrypoint.
- Cada path debe existir en el manifest interno `{path -> hash,size,mime}`.
- Open no-follow y regular file.
- `index.html`: `Cache-Control: no-store`.
- Assets con nombre/hash inmutable: `public, max-age=31536000, immutable`.
- ETag = SHA-256 del archivo.
- GET/HEAD y single-range opcional.
- MIME sale de allowlist, nunca de extension libre del producer.
- HTML/JS/CSS/SVG activos reciben policy especifica; SVG inline puede quedar deshabilitado beta.

### 11.4 API declarativa

Una route API resuelve una operation del manifest. `IO.web`:

1. autentica session cuando corresponde;
2. verifica tenant/app/deployment;
3. verifica Origin y CSRF para mutaciones;
4. parsea path/query/body bajo limites;
5. valida JSON Schema fail-closed;
6. obtiene binding autorizado por ID interno;
7. invoca una action catalogada;
8. aplica timeout, row/byte limits y redaction;
9. devuelve JSON shapeado o un BlobRef/public artifact para export grande.

El browser nunca puede elegir connector, statement, table, repository root ni target URL.

---

## 12. Sesion, origin y browser security

### 12.1 Bootstrap de Cloud

Flujo `cloud-session`:

```text
1. User OAuth session is valid in Fluxbee Cloud.
2. Cloud -> IO.cloud -> Admin: mint_web_access(app_id, tenant, subject).
3. Admin returns one-time access_code, TTL <= 120s.
4. Browser navigates:
   https://<app_key>.apps.<domain>/_fluxbee/session/exchange?code=<one-time>
5. IO.web validates signature/claims, app/tenant and consumes jti.
6. IO.web sets host-only session cookie and redirects to `/` without the code.
```

El exchange endpoint debe usar `Cache-Control: no-store` y no registrar query/code.

### 12.2 Cookie

```text
Set-Cookie: __Host-fb_session=<opaque>;
            Secure; HttpOnly; SameSite=Lax; Path=/
```

- Sin atributo `Domain`.
- Scope por app origin.
- Rotacion tras login y privilegio.
- Revocacion al undeploy/logout.
- TTL corto con refresh controlado.
- El browser JS no lee el token.

### 12.3 Dominio separado

Idealmente `apps.<domain>` usa un registrable domain distinto de Cloud/API. Ejemplo conceptual:

- Cloud: `cloud.fluxbee.example`;
- aplicaciones: `*.fluxbee-apps.example`.

Esto evita que una cookie `Domain=` del producto principal alcance contenido generado. Si se usa el
mismo parent domain, Cloud no debe emitir cookies parent-domain y el riesgo debe quedar aceptado.

### 12.4 CSP beta

Perfil fijo `web-app-v1`:

```text
default-src 'none';
script-src 'self';
style-src 'self' 'unsafe-inline';
img-src 'self' data: blob:;
font-src 'self' data:;
connect-src 'self';
form-action 'self';
object-src 'none';
base-uri 'none';
frame-src 'none';
worker-src 'none';
manifest-src 'none';
```

`frame-ancestors` se define por deployment policy (`'none'` o allowlist Cloud), nunca por HTML.
Scripts inline quedan bloqueados en beta; el bundle debe usar archivos JS propios. Estilos inline se
permiten para ergonomia, pero no pueden abrir red. Imports externos, CDN y analytics quedan bloqueados.

### 12.5 CSRF y CORS

- API same-origin; CORS default deny.
- Mutaciones requieren cookie de sesion + CSRF token asociado a session.
- `Origin` debe coincidir exactamente con canonical app origin.
- Requests sin Origin para mutaciones browser-like se rechazan, salvo clientes explicitamente
  catalogados fuera de esta beta.
- Preflight se responde desde policy fija, no reflejando origen arbitrario.

---

## 13. Bindings de datos

### 13.1 Principio

La AI programa contra capabilities de datos, no contra credentials ni infraestructura. Un alias del
manifest se resuelve a un `binding_id` concedido por Admin al tenant/app.

```text
manifest alias "analytics"
       -> binding_id bind:<uuid>
       -> connector profile reporting-readwrite-v1
       -> secret/config interno
       -> DB role/schema/repository root acotado
```

El registro durable de Admin contiene la relacion
`(binding_id, tenant_id, connector_type, resource_profile, config_ref, grants)`. Admin entrega esa
relacion cerrada a `IO.web`; ningun campo se toma del manifest salvo el alias. La entrada de Vault
queda dedicada al owner `IO.web@motherbee` y su value usa un schema por connector. `IO.web` conserva
solo el material minimo en memoria durante la vida del pool y lo descarta al revocar o liberar el
binding; persiste la referencia, no el value.

Este owner unico implica que el singleton beta puede leer todos los secrets explicitamente
dedicados a `IO.web`. La separacion efectiva entre tenants depende tambien de los grants de Admin,
las validaciones de metadata y pools separados. Reducir ese blast radius con identities/workers por
tenant o deployment queda como requisito de produccion, no se simula cambiando el payload.

### 13.2 PostgreSQL beta

Requisitos:

- role DB dedicado por app o tenant;
- schema/search_path fijo server-side;
- TLS hacia DB cuando no es local;
- credential solo desde `config_ref` owner-scoped en Vault;
- metadata de Vault y schema del value deben coincidir con el binding instalado por Admin;
- pool separado y con max connections;
- prepared statements o actions catalogadas;
- no SQL recibido del browser;
- no DDL, role management, COPY PROGRAM ni extensiones;
- statement timeout;
- transaction timeout;
- max rows y max response bytes;
- parametro siempre bind, nunca interpolacion;
- mutaciones dentro de transaction con rollback en error;
- audit de action ID, no de secrets ni datos sensibles.

En beta, el manifest no incorpora SQL generado. Referencia actions previamente aprobadas como
`report.read.v1`. Una futura policy `reviewed-sql-v1` requiere parser, review y sandbox separados.

### 13.3 Repositorios/datasets internos

El binding define un root/resource ID. El browser solo pasa parametros validados.

- no paths absolutos;
- canonical root + no-follow;
- no `..`, symlinks ni traversal por encoding;
- allowlist de extensiones/MIME;
- read-only por default;
- escritura requiere action explicita y transaccion/atomic rename;
- exports grandes se materializan como BlobRef y se publican por `IO.blob`, no se cargan completos en
  un response API.

### 13.4 HTTP connectors internos

Fuera de beta salvo allowlist fija. Si se agregan:

- base URL viene del binding, nunca del request;
- DNS/IP pinning y bloqueo de loopback/metadata endpoints;
- redirects deshabilitados o revalidados;
- methods/paths acotados;
- egress firewall;
- credentials owner-scoped.

---

## 14. Codigo generado por AI

### 14.1 Permitido beta

- HTML, CSS y JS de browser dentro del bundle;
- manifest declarativo;
- rutas API que referencian actions catalogadas;
- JSON Schema de parametros dentro de limites;
- composicion UI y logica cliente;
- SQL no; server code no.

### 14.2 Riesgos que permanecen

El JS de browser puede:

- consumir CPU/memoria del tab;
- mostrar UI enganosa;
- inducir al usuario a revelar informacion;
- realizar acciones API que el usuario realmente tenga autorizadas;
- contener errores logicos o supply-chain si se aceptan dependencias empaquetadas.

Mitigaciones:

- origin aislado;
- CSP fija;
- sin cookies Cloud;
- APIs con autorizacion server-side, CSRF y schemas;
- no confiar en validacion del frontend;
- no enviar credentials al JS;
- dependency inventory/hash en deployment;
- revocacion y audit.

### 14.3 Backend arbitrario futuro

Antes de ejecutar backend generado por AI se requiere como minimo:

- proceso/container por deployment;
- user namespace y usuario no-root;
- root filesystem read-only;
- seccomp/AppArmor o equivalente;
- sin Docker socket, host PID/network ni mounts de plataforma;
- CPU/RAM/PID/file quotas;
- egress deny-by-default;
- secret individual y DB role minimo;
- build sandbox sin secretos;
- dependency lock, scanning y provenance;
- kill/rollback administrado por Orchestrator;
- logs y audit por deployment.

Los managed nodes actuales no proveen por si solos ese aislamiento. Por eso no es parte de beta.

---

## 15. Lifecycle y consistencia

### 15.1 Estados

```text
requested -> staging -> ready -> activating -> active
                    \-> failed

active -> draining -> retained -> released
active -> revoked  -> draining -> released
active -> rollback_pending -> active(previous)
```

Solo un deployment puede estar `active` por `app_id` y `app_key`.

### 15.2 Readiness antes de visibilidad

`WEB_STAGE_RESPONSE ready=true` requiere:

- bundle y manifest validos;
- release hash persistido;
- static entrypoint servible;
- rutas API compiladas sin colision;
- bindings existentes y conectores inicializables;
- security profile valido;
- local HTTP health probe exitoso;
- deployment ledger durable.

Edge abre/switchea solo despues de ese ACK.

### 15.3 Switch y rollback

Edge aplica compare-and-swap:

```json
{
  "app_key": "...",
  "expected_deployment_id": "dep:old",
  "new_deployment_id": "dep:new",
  "release_hash": "..."
}
```

Requests nuevos van al nuevo deployment. Requests en vuelo conservan snapshot del viejo hasta
completar o alcanzar drain timeout. Si health falla inmediatamente despues del switch, Admin puede
CAS de vuelta al deployment retained anterior.

### 15.4 Unpublish ordering

Siempre:

1. close/switch route en Edge;
2. ACK de Edge;
3. invalidar sesiones;
4. drain `IO.web`;
5. release bindings/pools;
6. decrement release refcount;
7. delete posterior.

Nunca se borran archivos o DB bindings mientras la route publica sigue activa.

---

## 16. Proxy engine: codigo propio versus componente externo

La semantica de esta spec es independiente del engine. Opciones:

### 16.1 Hyper dentro de SY.edge

Recomendado para beta porque Edge ya usa Axum/Hyper y el upstream es unico/fijo.

- usar cliente Hyper/hyper-util con pooling y streaming;
- reutilizar rustls para mTLS;
- implementar solo policy, registry y header sanitation;
- no implementar parser/framing HTTP manual;
- tests adversariales obligatorios.

### 16.2 Caddy/Envoy como data plane

Valido si aparecen WebSockets, muchos upstreams o necesidades avanzadas. En ese modelo:

- `SY.edge` sigue siendo control/authority cache;
- genera una config acotada o usa API/xDS local autenticada;
- Caddy/Envoy no recibe datos libres del publisher;
- el admin API del proxy nunca es publico;
- config reload debe ser atomico y auditable.

Nginx/Caddy/Envoy no solucionan por si solos ownership, tenant, bindings ni lifecycle. Agregarlos
antes de necesitarlos aumenta procesos y drift. Traefik con acceso al Docker socket queda prohibido.

---

## 17. Infraestructura y packaging

### 17.1 Paquete Fluxbee

Cambios futuros:

- workspace/package `nodes/io/io-web`;
- build release en `scripts/install.sh` y `packaging/build-deb.sh`;
- `/usr/bin/io-web`;
- `io-web.service` con role gate motherbee;
- `/etc/fluxbee/io-web.env.example`;
- dirs state/web/releases/staging;
- enable/start/stop en postinst/firstboot/prerm;
- entrada exacta `IO.web` en `system_nodes.motherbee` y `wait_for`;
- tests que Identity/Vault no le asignan autoridad system;
- inventory/status/help operacional.

`IO.web` no entra en `dist/core` distribuido a workers mientras sea singleton motherbee. Una futura
version por worker requiere un perfil explicito, no copiarlo accidentalmente por estar en la lista.

### 17.2 Systemd hardening objetivo

- `User=fluxbee`, `Group=fluxbee`;
- `NoNewPrivileges=true`;
- `PrivateTmp=true`;
- `ProtectSystem=strict`;
- `ProtectHome=true`;
- `ReadWritePaths` solo state/releases/staging;
- capability bounding set vacio;
- address families solo UNIX/INET requeridas;
- limites de files/processes/memory;
- restart policy con backoff;
- credentials TLS 0600.

La implementacion debe verificar compatibilidad de estas directives con Blob/Vault/DB antes de
activarlas, no copiarlas a ciegas.

### 17.3 add_ingress

`add_ingress` debe:

1. resolver endpoint privado de `IO.web` desde infraestructura autorizada;
2. emitir client cert unico para ese ingress;
3. instalar CA/client cert/key en el ingress;
4. agregar client identity al trust de `IO.web`;
5. escribir `edge.web_apps` en hive.yaml remoto;
6. configurar firewall ingress -> IO.web solo puerto fijo;
7. instalar wildcard cert/domain de apps;
8. health probe mTLS;
9. persistir peer/cert IDs para teardown.

`remove_hive` revoca cert/client trust y elimina route cache. No toca deployments internos salvo que
ese ingress fuera su unica route y una policy explicita lo ordene.

### 17.4 Mapa de cambios por componente

| Componente/archivo | Cambio esperado |
|---|---|
| `nodes/io/io-web/` | Nuevo nodo, ledger, releases, listener mTLS, sesiones y connectors |
| `Cargo.toml` | Incorporar el package al workspace sin convertirlo en binario core |
| `crates/fluxbee_sdk/src/protocol.rs` | Constantes y payloads SYSTEM de `WEB_*` y `EDGE_*_WEB_APP` |
| `src/bin/sy_admin.rs` | Actions, ownership, bindings, ledger, orchestration y help estructurado |
| `src/bin/sy_edge.rs` | Registry web separado, Host routing y proxy Hyper al upstream fijo |
| `src/router/mod.rs` | Gates request/response exactos para los nuevos controles de Edge |
| `src/router/system_policy.rs` | Protected actions, autoridad y tests shadow del policy baked |
| `policy/system.rego` | Regla equivalente a la tabla Rust antes de regenerar `system.wasm` |
| `src/bin/sy_orchestrator.rs` | Lifecycle `IO.web`, service mapping, ingress config/certs y teardown |
| `src/bin/sy_identity.rs` | Allowlist exacta del packaged non-system node, sin ILK system |
| `src/bin/sy_vault.rs` / router identity | Cerrar derivacion autoritativa de caller ILK antes de bindings |
| `config/hive.yaml` | Desired state motherbee y configuracion local de `web` |
| `packaging/hive.yaml.example` | Defaults instalables equivalentes |
| `scripts/install.sh` | Build/install, unit, dirs, env y role gate |
| `packaging/build-deb.sh` | Contenido y maintainer scripts del paquete Fluxbee |
| `docs/onworking COA/archi/admin_help_reference.md` | Actions y ejemplos disponibles para Archi |
| `docs/onworking COA/archi/handbook_fluxbee.md` | Workflow de uso; no agregarlo al prompt base |

Los cambios de router, policy Rust, Rego y tests se entregan en el mismo commit funcional. No se
acepta una ventana donde el handler exista con un gate diferente al policy baked.

---

## 18. Observabilidad y audit

### 18.1 Logs

Campos permitidos:

- `request_id`;
- `app_id`, `deployment_id`;
- route/result code;
- method y path template, no query completa;
- status HTTP;
- latency y bytes;
- connector action ID;
- error code normalizado.

No loggear:

- cookies, Authorization, access codes;
- DB credentials o Vault values;
- request/response bodies por default;
- SQL parametrizado con valores;
- PII en URL/query;
- internal IP/port en errores publicos.

### 18.2 Metrics

- requests/inflight por app/deployment;
- status classes;
- body/response bytes;
- proxy latency y first-byte latency;
- upstream connection/TLS failures;
- API action latency/error;
- DB pool saturation/timeout;
- active sessions;
- deployments por state;
- route reconcile drift.

Metric labels no deben incluir app_key completo, subject, tenant name ni paths libres. Usar IDs
internos acotados o hashes truncados controlados.

### 18.3 Audit events

- app created;
- deployment staged/failed/activated;
- edge route opened/switched/closed;
- binding granted/revoked;
- access minted/session exchanged;
- rollback;
- policy violation;
- deployment released.

Audit registra actor router-stamped/Admin/Cloud authority y trace/request IDs.

---

## 19. Modelo de fallas

- **Admin cae durante stage:** deployment queda staging/ready sin route; reconcile decide release o
  continue idempotente.
- **IO.web cae:** Edge responde 502/503, nunca forwardea a otro host.
- **Edge cae:** apps no son publicas; Admin/Edge reconcile restaura rows.
- **DB cae:** API devuelve error acotado; static frontend puede seguir sirviendo.
- **Binding revocado:** IO.web cierra pool y operaciones devuelven 403/503 segun motivo.
- **Release corrupto:** deployment se marca failed/quarantined; no se repara con bytes no verificados.
- **Cert vencido/revocado:** mTLS falla cerrado; alerta operacional.
- **Clock drift:** access/session expiry requiere NTP; drift excesivo bloquea exchange.
- **App key filtrada:** no concede sesion privada; para app publica se rota/revoca route.
- **Publisher comprometido:** puede pedir deployments dentro de sus grants, no elegir tenant, binding,
  upstream ni secrets.
- **IO.web comprometido:** blast radius beta incluye apps/bindings que sirve. Produccion requiere
  particion por worker/tenant y secrets por principal.

---

## 20. Plan de pruebas obligatorio

### 20.1 Unit tests Edge

- app key/domain/Host valido e invalido;
- 64-char hex rechazado como label si no cumple formato beta;
- unknown/expired/revoked -> 404 uniforme;
- route CAS e idempotencia;
- no target host/port en row;
- header stripping e injection;
- duplicate/oversized headers;
- CL/TE ambiguity manejada por stack sin smuggling;
- CONNECT/TRACE/Upgrade rechazados;
- global/per-app semaphore;
- timeout/status mapping;
- no retry mutante;
- response header filtering y cookie policy.

### 20.2 Unit tests IO.web

- ZIP traversal, absolute path, backslash, symlink, hardlink y duplicates;
- zip bomb/file count/depth/size limits;
- manifest schema y route collisions;
- release hash determinista;
- staging nunca visible;
- static exact path/no listing/no-follow;
- SPA fallback solo cuando corresponde;
- reserved routes;
- schema validation;
- session/app/tenant mismatch;
- CSRF/Origin;
- anonymous mutation rechazada;
- binding no concedido/revocado;
- `config_ref` ausente, ajeno, de otro tenant o de connector incompatible falla cerrado;
- restart rehidrata secrets desde Vault por referencia y no desde disco;
- DB parameter binding, timeout, row/byte limits;
- release refcount y rollback.

### 20.3 Integration

- producer -> Admin -> IO.web stage -> Edge open -> browser GET;
- multi-file HTML/CSS/JS con `connect-src 'self'`;
- cloud access exchange -> cookie -> API -> DB;
- tenant A no accede app/binding de tenant B;
- malicious headers no llegan como trusted;
- direct request al listener IO.web sin mTLS falla;
- ingress cert equivocado/revocado falla;
- update sin downtime y old requests drain;
- rollback;
- undeploy cierra Edge antes de release;
- restart de Edge/IO.web/Admin converge;
- DB unavailable mantiene fail-closed.

### 20.4 Browser adversarial

- app A no puede leer origin/storage/cookies de app B;
- app no recibe cookies Cloud;
- external scripts/images/connect bloqueados;
- service worker bloqueado;
- iframe policy correcta;
- fake tenant headers ignorados;
- session cookie HttpOnly/host-only;
- CSRF cross-site bloqueado;
- XSS en datos no se vuelve HTML si renderer no lo solicita.

### 20.5 Lab Proxmox

- instalacion limpia motherbee + ingress;
- DNS/wildcard TLS de laboratorio;
- mTLS Edge -> IO.web;
- firewall impide otros peers al puerto;
- deploy/update/rollback/undeploy;
- restart de ambos hives;
- cert rotation/revocation;
- carga concurrente y backpressure;
- teardown ingress sin trust/cert/routes residuales.

---

## 21. Fases de implementacion propuestas

### Fase 0 - contratos

- congelar dominio publico y formato app_key;
- congelar access token encoding/signing rotation;
- definir binding catalog inicial;
- definir row/messages/actions y authority gates;
- cerrar el binding autoritativo `src_l2_name -> src_ilk` de Router/Vault y probar impersonation deny;
- threat review del proxy y DB connector.

### Fase 1 - IO.web static + proxy fijo

- package/lifecycle de `IO.web`;
- safe bundle extraction + manifest;
- private mTLS listener;
- Edge host registry + fixed upstream proxy;
- Admin deploy/status/undeploy;
- sitios multiarchivo sin API/bindings.

### Fase 2 - cloud-session

- mint one-time access;
- exchange/cookie/logout/revocation;
- origin/CSRF/CORS;
- Cloud integration.

### Fase 3 - API declarativa read-only

- action catalog;
- PostgreSQL/repository read bindings;
- schemas, limits, audit;
- export grande via BlobRef/IO.blob.

### Fase 4 - mutaciones

- CSRF obligatorio;
- transactions;
- action grants de write;
- idempotency keys;
- rollback y audit de negocio.

### Fase futura - backend aislado

- runtime/container por deployment;
- Orchestrator lifecycle y placement;
- network/secret isolation;
- solo despues permitir backend generado.

---

## 22. Decisiones que deben cerrarse antes de desarrollar

1. Dominio registrable dedicado para apps y estrategia wildcard cert.
2. Encoding y key rotation de `mint_web_access`.
3. Si `IO.web` beta vive solo en motherbee o en un worker dedicado.
4. Primer catalogo real de data actions/bindings.
5. DB role/schema provisioning y owner de secrets.
6. Limites finales de bundle/request/response/concurrency.
7. Si Fase 1 usa Hyper embebido o un proxy externo administrado.
8. Retention de releases y sesiones.
9. Politica `frame-ancestors` para embed en Cloud.
10. Que operaciones anonimas publicas se permiten, si alguna.
11. Implementacion exacta que deriva y valida el ILK efectivo para lecturas de Vault.

Estas decisiones son bloqueantes porque cambian seguridad/origin/autoridad. WebSockets, custom
domains y backend arbitrario no son bloqueantes: permanecen fuera de beta.

---

## 23. Definition of Done beta

La beta se considera completa solo cuando:

- `IO.web` esta empaquetado, lifecycle-managed e inventariable;
- Identity/Vault confirman que sigue siendo `IO.*`, no autoridad system;
- Vault rechaza un `src_ilk` forjado y deriva/valida owner desde identidad router-stamped;
- un producer real despliega bundle sin poder forjar tenant/upstream/binding;
- Edge sirve origin por app mediante proxy mTLS fijo;
- no existe acceso directo publico a `IO.web` ni a puertos de API;
- app privada requiere session y tenant correctos;
- app A no comparte origin/cookie/storage con app B;
- API usa binding autorizado y no acepta SQL/path/URL libre;
- update/rollback/undeploy son atomicos en el orden definido;
- Edge reimaging y restart convergen desde Admin/IO.web;
- pruebas adversariales, carga y Proxmox pasan;
- Admin help, handbook Archi, packaging docs y runbook operacional reflejan el contrato real;
- `IO.blob /public` y `/e/<ich>` mantienen compatibilidad sin regresiones.

Hasta entonces, el mecanismo soportado para contenido generado sigue siendo el HTML autocontenido
read-only de `io-blob-spec-v1.md`.
