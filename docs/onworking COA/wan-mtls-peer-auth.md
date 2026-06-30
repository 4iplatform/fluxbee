# Router WAN — mTLS peer authentication (diseño)

_2026-06-29. Cierra el gap de peer-auth en el canal **WAN** del router (`RT.gateway`),
que cruza la **frontera insegura** (red no confiable entre hives). Decisión del
operador: **mTLS** para el WAN; **CA dedicada**; identity `:9100` va aparte con
per-hive HMAC (fase 2). Survey + diseño: `wbp52a0mv` (workflow)._

## Problema
El handshake WAN (`handle_wan_connection`, `src/router/mod.rs` ~2411-2490) hace
HELLO + chequeo de allowlist (`authorized_hives`), pero el peer **auto-declara**
`hive_id`/`router_id` (`WanHelloPayload`) **sin prueba criptográfica**. La
allowlist dice *quién puede*, no *quién sos* — un nodo comprometido en la
allowlist puede reclamar cualquier `hive_id` y recibir todo el tráfico cross-hive
(orchestrator config, vault broadcasts, ruteo). El WAN además va **en claro**.
mTLS resuelve las dos cosas: **autenticación mutua** (prueba de identidad por
cert) **+ cifrado/integridad** sobre la frontera insegura.

## Anclaje de confianza
El único anclaje cripto hoy es el **bootstrap SSH**: en `add_hive` motherbee
autentica al nodo (creds del payload) y le siembra `motherbee.key`. Ese canal
autenticado es donde se distribuyen los certs del mesh.

## Diseño

### CA del mesh (dedicada)
- Motherbee genera **una CA propia** en el primer boot (no derivada de
  `motherbee.key` — aislamiento: comprometer el SSH no compromete la CA):
  - `ca.key` (clave privada de la CA) + `ca.crt` (cert self-signed de la CA),
    en `/var/lib/fluxbee/tls/ca/` (`0600` la key).
  - Generados con `rcgen` (ECDSA P-256 o Ed25519). Larga validez (p.ej. 10 años).
- Motherbee emite su **propio leaf** `CN=motherbee` firmado por la CA →
  `/var/lib/fluxbee/tls/motherbee/{cert.crt,cert.key}`.

### Emisión de cert por-hive (en `add_hive`)
En el flujo `add_hive`/`add_egress_hive_flow` (`sy_orchestrator.rs` ~16173), tras
el bootstrap SSH (canal autenticado), motherbee:
1. Genera un **leaf cert** para el hive nuevo: `CN=<hive_id>`, SAN incluye
   `<hive_id>`, firmado por la CA del mesh. Validez media (p.ej. 1 año; rotación
   futura).
2. Distribuye sobre SSH (`write_remote_file`, ya endurecido) a
   `/var/lib/fluxbee/tls/<hive_id>/`: `ca.crt` (para validar peers), `cert.crt` y
   `cert.key` (`0600`) del hive.
3. (No va al vault: el WAN levanta antes que el vault esté disponible — los certs
   tienen que estar en disco al arrancar el router.)

### Transporte TLS en el WAN (`src/router/mod.rs`)
- Crates: `rustls` + `tokio-rustls` + `rcgen` + `rustls-pemfile`.
- `wan_listen_loop`: el `TcpStream` aceptado se envuelve con un
  `tokio_rustls::TlsAcceptor` (rol **servidor**, presenta el leaf del hive,
  **requiere** cert de cliente). `wan_connect_loop`: el `TcpStream` saliente se
  envuelve con `TlsConnector` (rol **cliente**, presenta el leaf, valida el del
  servidor). El resto del protocolo (HELLO/LSA/frames) corre **encima** del
  stream TLS, sin cambios de lógica.
- **Verificación de peer** (custom verifier, ambos lados):
  1. el cert del peer está firmado por la **CA del mesh** (cadena válida);
  2. el `CN`/SAN del peer == el `hive_id` que declara en el HELLO **y** está en
     `authorized_hives` (la allowlist se mantiene como capa de autorización; mTLS
     agrega la **autenticación** que faltaba).
- Cierra forja cross-hive: worker2 no tiene la `cert.key` de worker1 → no puede
  presentarse como worker1.

### Feature flag + migración (sin romper la malla en rollout)
- Flag `wan.mtls` en `hive.yaml` (`disabled` | `permissive` | `required`),
  propagable por `CONFIG_CHANGED`:
  - **disabled**: comportamiento actual (TCP plano). Default hasta el rollout.
  - **permissive**: acepta TLS y plano (detección por el primer byte: 0x16 =
    TLS handshake). Para convivir nodos viejos/nuevos durante el deploy.
  - **required**: solo TLS; rechaza plano. Estado final.
- Rollout: (1) deploy del código nuevo con `disabled`/`permissive`; (2) re-emitir
  certs a todos los hives (re-correr add_hive o un comando de emisión); (3)
  motherbee pasa a `required` vía `CONFIG_CHANGED`.
- Un nodo sin certs en disco loguea WARN y se comporta como `disabled` localmente
  (no rompe el arranque).

### Compatibilidad con el ingress (RT.edge, futuro)
RT.edge (ingress público, `edge-control-protocol.md` — aún no escrito; descrito en
`edge-egress-nat-spec.md` §10.3) maneja **terminación HTTPS + material TLS**. La
CA del mesh + la infra de certs que monta este diseño es **reutilizable** por
RT.edge (emisión de certs de servidor para la terminación pública). No se
implementa acá, pero el diseño no lo cierra.

## Fuera de alcance (fases siguientes)
- **Identity `:9100`** — per-hive HMAC-SHA256 (canal interno; fase 2, decidido).
- **Rotación automática de certs** (vía vault, post-bootstrap).
- **Revocación** (CRL/OCSP) — por ahora, rotación de CA + re-emisión.
- **RT.edge ingress** — usa esta CA, spec aparte.

## Validación (lab) — HECHA 2026-06-29
motherbee + worker1, `wan.mtls=required` en ambos:
- CA dedicada generada en motherbee (`/var/lib/fluxbee/tls/ca`); leaf de motherbee
  (`CN=motherbee`, `SAN DNS:motherbee`) y de worker1 (`CN=worker1`) firmados por
  la CA. El leaf de worker1 se emitió con `examples/mesh_cert_tool` desde la CA
  (el worker del lab ya estaba provisionado sin certs; `add_hive` lo haría en un
  alta nueva).
- Ambos routers reportan **`wan mtls mode = required`** (certs cargados, no
  degradado). La malla levanta sobre mTLS: motherbee ve `worker1: connected` (el
  HELLO/LSA completó con `required` activo, que rechaza plano).
- **Adversarial**: un peer **plano** a motherbee:9000 recibe un **TLS alert**
  (`0x15 03 03 …`, fatal) en el handshake — nunca llega al HELLO. (mesh_tls unit
  tests ya cubren además el rechazo de cert de **CA ajena** y el round-trip mutuo.)
- Re-handshake: reiniciar el router de worker1 reconecta limpio.

Gotcha de deploy (anotado): al desplegar binarios nuevos al lab/VM hay que
regenerar `dist/core/manifest.json` (sha256+size) o el orchestrator crash-loopea
en la validación de manifest **antes** de generar la CA.

Tests unitarios (mesh_tls, 5): verifier CA-only (handshake mutuo + rechazo de CA
ajena), emisión/reload de CA, extracción de hive del cert.
