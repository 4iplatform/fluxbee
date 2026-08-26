# Fluxbee - 07 Operaciones (Deploy y Ciclo de Vida)

**Estado:** v2 (deploy model actual)
**Reescrito:** 2026-07-21 para reflejar la realidad actual del código (`scripts/install.sh`, `packaging/build-deb.sh`, `packaging/deb-postinst`, `packaging/fluxbee-firstboot`, `src/bin/sy_orchestrator.rs`, `src/bin/sy_admin.rs`).
**Audiencia:** Ops/SRE, deployment.

> **Aviso de reescritura:** Esta versión reemplaza el modelo de bootstrap muerto que documentaba
> la versión anterior (`BOOTSTRAP_SSH_USER=administrator` / `BOOTSTRAP_SSH_PASS=magicAI`
> hardcodeados). Ese modelo **ya no existe**. Hoy el bootstrap es **credenciales-en-payload**
> por la API de admin (`POST /hives`), con `ssh_user` obligatorio y sin usuario/contraseña
> fijos en el binario.

---

## 1. Modelo de deployment (resumen)

Fluxbee se instala en **un solo nodo**: la **motherbee**. Todo lo demás (workers, egress,
ingress) son cajas Linux limpias (solo SSH) que la motherbee **empuja** ("vendor-push") por
`add_hive`. Los spokes **nunca** se instalan con el `.deb`.

```
                    ┌──────────────────────────────────────────┐
                    │              MOTHERBEE                     │
   apt-get install  │  (único nodo instalado con el .deb)        │
   ./fluxbee.deb ──▶ │  PostgreSQL (Depends del paquete)          │
   sudo fluxbee-    │  SY.* core + SY.vault + SY.admin           │
   firstboot        │  IO.cloud / IO.blob (runtimes managed)           │
                    │  RT.gateway :9000 (WAN, mtls=required)     │
                    │  /var/lib/fluxbee/ssh/motherbee.key        │
                    └──────────────────────────────────────────┘
                                     │  POST /hives (add_hive)
                                     │  credenciales-en-payload + SSH bootstrap
             ┌───────────────────────┼───────────────────────┐
             ▼                       ▼                       ▼
      ┌────────────┐          ┌────────────┐          ┌────────────┐
      │  WORKER    │          │  EGRESS    │          │  INGRESS   │
      │ (Linux+SSH)│          │ (Linux+SSH)│          │ (Linux+SSH)│
      │ SIN .deb   │          │ SIN .deb   │          │ SIN .deb   │
      │ SIN postgres│         │ NAT saliente│         │ SY.edge :443│
      └────────────┘          └────────────┘          └────────────┘
       vendor-push             vendor-push             vendor-push
```

Puntos clave (todos verificados contra el código):
- **DB centralizada en la motherbee.** Los spokes NO tienen PostgreSQL. `SY.storage` y
  `SY.vault` corren solo en motherbee; `SY.identity` corre en motherbee (primary, escribe DB) y
  en worker (réplica en SHM, sin DB).
- **SSH es solo bootstrap.** Tras un join exitoso el default (`ssh_access:"revoke"`) borra la
  llave de bootstrap de la motherbee y el grant de sudoers del spoke. La gestión diaria es por
  socket del router + dist-sync, no por SSH.
- **WAN mTLS requerido por default** (`wan.mtls: required` en el template de motherbee).

---

## 2. INSTALL — solo motherbee

### 2.1 Construir el paquete

```bash
# En un host con el toolchain (rust + go + protoc):
packaging/build-deb.sh 0.1.0
# → dist/fluxbee_0.1.0_amd64.deb
```

`build-deb.sh` separa BUILD (aquí) de INSTALL (en el target). Compila el core Rust
(`rt-gateway`, `sy-admin`, `sy-config-routes`, `sy-architect`, `sy-vault`, `sy-orchestrator`,
`sy-storage`, `sy-identity`, `sy-cognition`, `sy-policy`, `sy-edge`), los binarios Go
(`sy-opa-rules`, `sy-timer`, `sy-wf-rules`, `wf-generic`), `sy-frontdesk-gov`, los runtimes managed
`io-cloud`/`io-blob`, y siembra el runtime instanciado `io.api` bajo `dist/runtimes`. **Hornea los
hashes del manifiesto `dist/core` en build-time** (evita el crash-loop de "manifest hash
mismatch" que causaba una copia manual de binarios).

### 2.2 Instalar en la motherbee

```bash
sudo apt-get install ./fluxbee_0.1.0_amd64.deb
```

El control del `.deb` declara `Depends: adduser, openssl, libc6 (>= 2.39), postgresql` — **PostgreSQL
es ahora un `Depends` duro**, así que `apt-get install` lo trae automáticamente. El `postinst`:
- crea el usuario de sistema `fluxbee` y los directorios de estado;
- copia `hive.yaml.example` → `hive.yaml` si no existe (el operador lo edita antes de firstboot);
- `daemon-reload` + `enable` de `sy-orchestrator`, `io-cloud`, `io-blob`;
- en una **instalación fresca NO arranca servicios** (arranca solo en upgrade, cuando dpkg pasa el
  argumento de versión previa). El transition a "encendido" lo hace `fluxbee-firstboot`.

### 2.3 Editar hive.yaml y correr firstboot

```bash
sudo nano /etc/fluxbee/hive.yaml       # editar hive_id, wan.listen, wan.mtls, etc.
sudo fluxbee-firstboot
```

`fluxbee-firstboot` resuelve el problema huevo-gallina del Model D' (el orchestrator no
estabiliza hasta que el secreto de postgres está en el vault, pero el vault necesita el
orchestrator arriba para recibirlo). Pasos (idempotente):

1. **PostgreSQL + DB bootstrap** — arranca postgres y crea el rol `fluxbee` y las DBs
   `fluxbee`, `fluxbee_identity`, `fluxbee_storage`.
2. **Arranca `sy-orchestrator`** (crash-loopea hasta que aterriza el secreto).
3. **Espera la admin API y hace `vault_put` del secreto de postgres**:
   `POST /hives/<hive>/vault/secrets` con `key=storage_postgres_url`,
   `value={postgres_url: "postgresql://fluxbee:fluxbee@127.0.0.1:5432"}`,
   `metadata.resource_type=postgres` bajo el root tenant.
4. **Reconecta los consumidores de DB** — reinicia `sy-vault` PRIMERO (arranca con el secreto ya
   persistido), luego `sy-storage`/`sy-identity` (cada uno hace *pull* del secreto del vault vivo
   al arrancar), y re-patea el orchestrator hasta que el hive quede ready.
5. **Espera hive ready y auto-spawnea los runtimes managed de boot** `io-cloud`, `io-blob`.

Variables de entorno del firstboot: `FLUXBEE_DB_USER`/`FLUXBEE_DB_PASSWORD` (default
`fluxbee`/`fluxbee`), `FLUXBEE_ADMIN` (default `127.0.0.1:8080`).

Al terminar, `scripts/install.sh` (build+deploy desde fuente, alternativa al `.deb`) genera la
llave de bootstrap de la motherbee si no existe:

```
/var/lib/fluxbee/ssh/motherbee.key        # ed25519, 0600 (privada)
/var/lib/fluxbee/ssh/motherbee.key.pub    # 0644 (pública sembrada en cada spoke)
```

> `scripts/install.sh` es la ruta build-from-source (build determinista + install + restart en el
> orden Model D'); el `.deb` es la ruta empaquetada. El set de nodos base (core + IO/AI) que
> ambos siembran en `dist/` se define en `packaging/base-nodes.json` (fuente única de verdad);
> ver [packaging-and-build.md](packaging-and-build.md).

---

## 3. hive.yaml (motherbee)

El template empaquetado (`packaging/hive.yaml.example`) es una motherbee fresca:

```yaml
hive_id: "motherbee"
role: motherbee

wan:
  listen: "0.0.0.0:9000"
  gateway_name: "RT.gateway"
  mtls: required          # disabled | permissive | required (default del template)

nats:
  mode: embedded
  port: 4222

admin:
  listen: "127.0.0.1:8080"

architect:
  listen: "0.0.0.0:3000"

storage:
  path: "/var/lib/fluxbee"

identity:
  sync:
    port: 9100
    auth: required        # HMAC per-hive en el canal :9100 (la motherbee distribuye la key en add_hive)

government:
  identity_frontdesk: "SY.frontdesk.gov@motherbee"

blob:
  enabled: true
  path: "/var/lib/fluxbee/blob"
  sync:
    enabled: true
    public_enabled: true  # canal público one-way: motherbee envía, ingress recibe
    tool: "syncthing"
    api_port: 8384
    data_dir: "/var/lib/fluxbee/syncthing"

dist:
  path: "/var/lib/fluxbee/dist"
  sync:
    enabled: true
    tool: "syncthing"

system_nodes:
  motherbee:  { nodes: [...], wait_for: [...] }   # ver template
  worker:     { nodes: [...], wait_for: [...] }
  egress:     { nodes: [SY.config.routes], wait_for: [SY.config.routes] }
  ingress:    { nodes: [SY.config.routes, SY.edge], wait_for: [SY.config.routes] }
```

### 3.1 WAN mTLS

`wan.mtls` controla la autenticación del transporte inter-hive:
- `disabled` — WAN plano.
- `permissive` — mTLS cuando ambos peers presentan cert, acepta plano (WARN).
- `required` — rechaza cualquier peer sin cert de malla válido (fail-closed). **Default del
  template.**

El instalador provisiona el CA de malla + el cert de este hive bajo
`/var/lib/fluxbee/tls/<hive_id>` en cualquier modo, así que cambiar de modo es instantáneo. **Si
está en `required`, cada hive que se una también debe correr `required`** — `add_hive` distribuye
su cert; un join plano sería rechazado.

### 3.2 Orden de arranque (Model D')

El orden de `system_nodes.motherbee` es cargado por `sy_orchestrator`:
1. `SY.config.routes` PRIMERO (writer de `/jsr-config-<hive>` en SHM).
2. Todos los consumidores del vault DESPUÉS (se registran con el router antes de que el vault
   emita su broadcast de bootstrap; cada uno tolera `VAULT_UNAVAILABLE`).
3. `SY.vault` ÚLTIMO — los broadcasts del router solo alcanzan nodos ya registrados.

---

## 4. Los 4 roles

| Rol | Cómo se crea | PostgreSQL | Nodos de sistema (start order) |
|-----|--------------|:----------:|--------------------------------|
| **motherbee** | `.deb` + `fluxbee-firstboot` | ✓ (Depends) | config.routes, identity, opa.rules, admin, IO.blob, architect, storage, cognition, policy, timer, wf-rules, frontdesk.gov, vault |
| **worker** | `add_hive role=worker` | ✗ | config.routes, identity(réplica SHM), opa.rules, cognition, policy, timer, wf-rules |
| **egress** | `add_hive role=egress` + `egress{}` | ✗ | config.routes (+ NAT saliente por nft) |
| **ingress** | `add_hive role=ingress` + `ingress{}` | ✗ | config.routes, SY.edge (:443 público, fail-closed) |

`add_hive` **rechaza `role=motherbee`** (la motherbee solo se crea por `.deb`); acepta
`worker` (default), `egress`, `ingress`.

Ownership de DB (excepción de identity): `SY.storage` es el writer de persistencia cognitiva;
`SY.identity` PRIMARY escribe directo su dominio (`identity_tenants/ilks/ichs/vocabulary/
ilk_aliases`) porque el registro de identidad requiere confirmación síncrona. Los workers
**nunca** escriben DB de identity: aplican réplica por socket (:9100, HMAC) y mantienen SHM local.

---

## 5. add_hive — vendor-push de un spoke

### 5.1 API

```
POST /hives
Content-Type: application/json
```

Ruteo interno: `SY.admin` → `SY.orchestrator@motherbee` (action `add_hive`).

### 5.2 Campos del payload (esquema real de `sy_admin.rs`)

**Requeridos:**

| Campo | Tipo | Descripción |
|-------|------|-------------|
| `hive_id` | string | Id único del hive a crear. |
| `address` | string | Dirección WAN/bootstrap alcanzable desde la motherbee. |
| `ssh_user` | string | Login admin en la caja vacía para el bootstrap inicial. **Requerido, sin default.** Debe matchear `^[a-z_][a-z0-9_-]*$` (máx 32). |

**Opcionales:**

| Campo | Tipo | Descripción |
|-------|------|-------------|
| `ssh_password` | string | Password admin para el bootstrap de una caja vacía. Solo se consulta si la llave de bootstrap aún no está sembrada y no se pasó `ssh_key`. Nunca se loguea ni se persiste. |
| `ssh_key` | string | Llave privada SSH de bootstrap (PEM, **sin cifrar**). Canal key-first para una imagen cloud (authorized key inyectada por cloud-init, `PasswordAuthentication` OFF en el server): siembra la llave de la malla sin password del server. Rechaza llaves con passphrase (`ENCRYPTED`). Nunca se loguea ni se persiste. |
| `ssh_access` | string | Postura SSH post-join. `revoke` (default) = SSH es solo-bootstrap: se borra la llave de la motherbee + el grant de sudoers. `key_only_persist` = deja SSH abierto solo-por-llave (password off) vía una **llave per-spoke de recuperación** guardada en `SY.vault` bajo `ssh:<hive_id>`. |
| `role` | string | `worker` (default), `egress`, `ingress`. |
| `egress` | object | Requerido si `role=egress`: `lan_cidr` (req), `wan_iface` (req), `lan_iface` (req), `edge_ip` (opt, default = primera IP usable de `lan_cidr`), `ipv6` (opt, solo `"blocked"`). |
| `ingress` | object | Requerido si `role=ingress`: `listen` (req, `host:port`), `tls_vault_key` + `vault_hive` para HTTPS, o `allow_plaintext=true` solo para un listener de desarrollo explícito fuera de :443. |
| `harden_ssh` | bool | Endurece SSH tras bootstrap. |
| `restrict_ssh` | bool | Restringe el acceso SSH de bootstrap (from-only) tras provisioning. |
| `require_dist_sync` | bool | Exige readiness de dist-sync antes de responder ok. Ignorado para `role=egress`. |
| `dist_sync_probe_timeout_secs` | u64 | Timeout del probe de dist-sync (5..600). |

### 5.3 Bootstrap key-first (sin administrator/magicAI)

El orchestrator siempre intenta **primero la llave** (`/var/lib/fluxbee/ssh/motherbee.key`):

1. Probe `ssh_with_key(address, motherbee.key, ssh_user)`. Si funciona, la caja ya está sembrada;
   sigue key-first.
2. Si el probe falla, **siembra** la llave de la motherbee, en este orden de preferencia:
   - `ssh_key` del operador (canal cloud-image: funciona aunque el server tenga
     `PasswordAuthentication=no`), o
   - `ssh_password` (canal password), o
   - si no hay ninguno → error `SSH_KEY_FAILED` (nunca hay fallback hardcodeado).
3. Asegura el sudoers del orchestrator remoto, verifica `sudo -n`, y ya opera todo por la llave.
4. Empuja los binarios del core (desde `dist/core/bin` + manifiesto), escribe `hive.yaml` del
   spoke (uplink a la motherbee, sin `wan.listen`, sin `sy-admin`), instala units y arranca
   `sy-orchestrator` remoto, que conecta por WAN.

El bootstrap tiene **retry transitorio** (`SSH_TRANSIENT_RETRIES=4`, base 800 ms): un fallo
transitorio del socket SSH salta la revocación de la llave de bootstrap (`revoke_skipped_transient`)
para que el retry siga siendo key-first en vez de dejar el spoke a medio-hacer.

### 5.4 Postura post-join

- **`revoke` (default):** tras join exitoso, `best_effort_revoke_bootstrap` borra la llave de la
  motherbee de `authorized_keys` del spoke y remueve el grant de sudoers. El spoke queda sin
  acceso SSH permanente. La reconciliación posterior llega por socket del router.
- **`key_only_persist`:** `finalize_spoke_key_persist` genera una llave **per-spoke** nueva,
  la agrega a `authorized_keys`, apaga password auth, y hace un **verify-before-revoke**: escribe
  la privada a un scratch 0600, prueba `sudo -n` con ella, y solo entonces persiste la privada en
  `SY.vault` bajo `ssh:<hive_id>` y borra la llave de la motherbee. Si el verify falla, **mantiene
  la llave de la motherbee** (nunca deja al spoke sin acceso) y responde en modo
  `degraded_kept_bootstrap`. La reconciliación de un hive `key_only_persist` lee esa llave
  per-spoke del vault (cierra la contradicción reconcile↔revoke).

### 5.5 Egress e ingress (particularidades)

- **Egress** (`add_egress_hive_flow`): valida `egress{}` eagerly (resuelve la config de NAT en
  request-time, con validación de nombres de interfaz), corre un core mínimo
  (`SY.config.routes` + NAT nft). No corre dist-sync (`require_dist_sync` se ignora).
- **Ingress** (`add_ingress_hive_flow`): corre `SY.config.routes` + `SY.edge`. `SY.edge` bindea
  :443 (fail-closed en `role==ingress`) y proxifica `/e/<ich>` hacia la malla. **No** tiene
  `SY.identity` (frontera de identidad): forwardea por nombre de owner pre-resuelto (Option Z).
  El cert TLS se siembra primero en `SY.vault` (`tls_vault_key`, owner
  `SY.edge@<edge_hive>`); el edge lo lee al arrancar.

---

## 6. Ciclo de vida

### 6.1 Upgrade del `.deb` en la motherbee

```bash
sudo apt-get install ./fluxbee_<nueva-version>_amd64.deb
```

- `prerm` para y deshabilita el orchestrator + todos los `sy-*` en orden inverso.
- `postinst` instala binarios + units nuevos y, **como es upgrade** (dpkg pasa la versión previa),
  arranca de nuevo `sy-orchestrator`, `io-cloud`, `io-blob`.
- Los units tienen `TimeoutStopSec=15`, así que un stop/restart/upgrade no cuelga 90 s hasta el
  SIGKILL del default de systemd. Los binarios salen rápido con SIGTERM y **`sy-orchestrator` ya
  no tira el hive abajo al salir** — el resto de la malla sigue arriba durante el upgrade.
- **No** se re-corre `fluxbee-firstboot` (el secreto de postgres ya está en el vault).

### 6.2 Propagación de core-update a los spokes

Un spoke no se re-instala; recibe binarios nuevos por dist-sync:
1. La motherbee publica el core nuevo en `/var/lib/fluxbee/dist/core/bin` + `manifest.json`
   (hashes reales).
2. Syncthing replica `dist/` a los spokes (`sendonly` en motherbee, `receiveonly` en spokes).
3. `POST /hives/{id}/update {category:"core", manifest_version, manifest_hash}` envía
   `SYSTEM_UPDATE` al `SY.orchestrator@{hive}`.
4. El orchestrator del spoke valida el manifiesto local, **swap-ea los binarios, re-renderiza los
   units de su rol (`regen_local_core_units`), hace `daemon-reload` y reinicia** — así los binarios
   nuevos arrancan bajo los units nuevos. Cambios de unit (p. ej. `TimeoutStopSec`, edición de
   dependencias) viajan por esta ruta `category=core`. Además el bootstrap del spoke re-genera sus
   units para matchear el template actual (self-heal).

Categorías de update: `runtime`, `core`, `vendor`. El único contrato de update remoto es
`SYSTEM_UPDATE` (no hay watchdog SSH de runtime/core/vendor).

### 6.3 Recuperación por reboot (sin re-firstboot)

- **Motherbee:** el postinst deja `enabled` **solo** `sy-orchestrator` (io-cloud/io-blob son runtimes managed que el orchestrator auto-spawnea al boot
  `io-cloud`/`io-blob`); al bootear, `sy-orchestrator` arranca solo y **levanta el resto del core**
  (`systemctl start`) en orden Model D'. El vault ya tiene el secreto de postgres persistido,
  storage/identity hacen pull. **No** hace falta re-correr `fluxbee-firstboot`.
- **Spoke:** `sy-orchestrator` está `enabled`; al bootear reconecta por WAN a la motherbee
  (~15 s típico) y re-sincroniza identity/config/opa. Sin estado que reconstruir salvo caches
  locales (LanceDB/jsr-memory) que se regeneran.

---

## 7. Ejemplo completo (worked example)

```bash
BASE="http://127.0.0.1:8080"

# --- 0. Motherbee ya instalada (.deb + fluxbee-firstboot). Verificar: ---
curl -sS "$BASE/hives" | jq .       # debe incluir "status":"ok"

# --- 1. Worker con password (caja vacía, PasswordAuthentication=yes) ---
curl -sS -X POST "$BASE/hives" -H 'content-type: application/json' -d '{
  "hive_id": "worker-1",
  "address": "192.168.8.221",
  "ssh_user": "ubuntu",
  "ssh_password": "<bootstrap-pass>",
  "role": "worker",
  "harden_ssh": true
}' | jq .

# --- 2. Worker sobre imagen cloud (key-first, password del server OFF) ---
#     ssh_user con sudo passwordless (default cloud-init); llave PEM sin cifrar.
curl -sS -X POST "$BASE/hives" -H 'content-type: application/json' -d "{
  \"hive_id\": \"worker-2\",
  \"address\": \"10.0.0.30\",
  \"ssh_user\": \"ubuntu\",
  \"ssh_key\": \"$(sed ':a;N;$!ba;s/\n/\\n/g' ~/.ssh/cloud_image_key)\",
  \"role\": \"worker\",
  \"ssh_access\": \"key_only_persist\"
}" | jq .
# key_only_persist → llave per-spoke de recuperación queda en SY.vault bajo ssh:worker-2

# --- 3. Egress (NAT saliente) ---
curl -sS -X POST "$BASE/hives" -H 'content-type: application/json' -d '{
  "hive_id": "egress-1",
  "address": "192.168.8.230",
  "ssh_user": "ubuntu",
  "ssh_password": "<bootstrap-pass>",
  "role": "egress",
  "egress": { "lan_cidr": "192.168.8.0/24", "wan_iface": "eth0", "lan_iface": "eth1" }
}' | jq .

# --- 4. Ingress (puerta pública SY.edge :443) ---
# Sembrar el cert TLS primero:
curl -sS -X POST "$BASE/hives/motherbee/vault/secrets" -H 'content-type: application/json' -d '{
  "key": "edge_tls_fluxbee_ai",
  "value": { "cert": "<PEM chain>", "key": "<PEM key>" },
  "metadata": { "resource_type": "tls", "owner_node": "SY.edge@ingress-1" }
}' | jq .
curl -sS -X POST "$BASE/hives" -H 'content-type: application/json' -d '{
  "hive_id": "ingress-1",
  "address": "203.0.113.10",
  "ssh_user": "ubuntu",
  "ssh_password": "<bootstrap-pass>",
  "role": "ingress",
  "ingress": { "listen": "0.0.0.0:443", "tls_vault_key": "edge_tls_fluxbee_ai", "vault_hive": "motherbee" }
}' | jq .
```

Verificar un join:
```bash
curl -sS "$BASE/hives/worker-1" | jq .              # status
curl -sS "$BASE/versions?hive=worker-1" | jq .      # core/runtime/vendor
```

---

## 8. Recuperación / troubleshooting

### 8.1 add_hive falla en el bootstrap SSH
- `ssh_user` es obligatorio y debe matchear `^[a-z_][a-z0-9_-]*$`.
- Caja vacía sin llave sembrada: hace falta `ssh_password` **o** `ssh_key`. Si el server tiene
  `PasswordAuthentication no`, usar `ssh_key` (canal cloud-image).
- `ssh_key` debe ser PEM **sin cifrar** (un key con passphrase se rechaza: el bootstrap no
  interactivo no puede aportar la passphrase).
- Fallo transitorio de socket: el flujo reintenta (4x, base 800 ms) manteniéndose key-first.
- **`ssh_password` "no anda" en imagen cloud:** Ubuntu cloud-image trae
  `/etc/ssh/sshd_config.d/50-cloud-init.conf` con `PasswordAuthentication no`, y sshd es
  *first-match*: un drop-in `99-*.conf` **no** lo pisa. Preferí `ssh_key` (la llave que cloud-init
  ya inyectó); si insistís con password, poné un drop-in de número **menor** (`00-*.conf`). El
  bootstrap ahora reporta `SSH_AUTH_FAILED` con este hint (antes decía `SSH_KEY_FAILED` aun usando
  password).

### 8.0 Contrato del clean box (spoke) — prerequisitos

Un spoke es una **caja Linux limpia con SOLO SSH**, bootstrapeada por la motherbee. Antes del
`add_hive` la caja debe tener:
- **sshd activo** y alcanzable desde la motherbee en el `address` indicado.
- **un usuario sudo** (`ssh_user`) con sudo passwordless.
- **acceso (elegí uno):** *(recomendado)* la **pubkey del operador/cloud-init en
  `authorized_keys`** → bootstrap **key-first** (ejemplo worker-2 arriba), independiente de
  `PasswordAuthentication`; **o** `PasswordAuthentication yes` (drop-in `00-*.conf` que gane al
  `50-cloud-init.conf`) + `ssh_password`.
- Una **imagen cloud estándar de Ubuntu** ya cumple esto (sshd on + usuario `ubuntu` + llave
  inyectada). Una imagen/plantilla propia debe habilitar sshd + crear el usuario (ver
  `lab/template-prep.sh` para el patrón del lab).

### 8.2 Recuperar un spoke `key_only_persist`
La llave per-spoke de recuperación está en `SY.vault` bajo `ssh:<hive_id>`. La reconciliación del
orchestrator la usa automáticamente para alcanzar el spoke (el bootstrap de la motherbee ya fue
revocado). Para acceso manual, materializarla desde el vault a un scratch 0600 y
`ssh -i <scratch> <ssh_user>@<address>`.

### 8.3 Recuperar un spoke `revoke` (SSH cerrado)
No hay llave permanente. Recuperar por consola out-of-band (hipervisor), reinstalar la pública de
la motherbee en `authorized_keys` del `ssh_user`, y re-reconciliar por API
(`DELETE /hives/{id}` seguido de `POST /hives` con las mismas credenciales).

### 8.4 El hive no queda ready tras firstboot
`fluxbee-firstboot` es idempotente: re-correrlo. Revisar postgres (`systemctl status postgresql`),
que el secreto esté en el vault (`GET /hives/<hive>/vault/secrets`), y los logs
(`journalctl -u sy-orchestrator -f`, `-u sy-vault`, `-u sy-storage`).

### 8.5 Comandos de diagnóstico
```bash
journalctl -u sy-orchestrator -f
systemctl is-active rt-gateway sy-orchestrator sy-identity sy-admin sy-vault sy-storage
cat /etc/fluxbee/hive.yaml
```

---

## 9. API de admin (orchestrator/hives) — referencia

Estas rutas siguen vigentes (`SY.admin`, default `127.0.0.1:8080`, override `JSR_ADMIN_LISTEN`).

### 9.1 Hives
| HTTP | Action | Descripción |
|------|--------|-------------|
| `POST /hives` | `add_hive` | Vendor-push de un spoke (ver §5). |
| `GET /hives` | `list_hives` | Lista hives + health del hive local. |
| `GET /hives/{id}` | `get_hive` | Info de un hive. |
| `DELETE /hives/{id}` | `remove_hive` | Cleanup remoto por socket; si el spoke no es alcanzable, cleanup local-only. |
| `POST /hives/{id}/update` | `update` | `SYSTEM_UPDATE` (`runtime`/`core`/`vendor`). |
| `POST /hives/{id}/sync-hint` | `sync_hint` | `SYSTEM_SYNC_HINT` (`blob`/`dist`). |
| `POST /hives/{id}/vault/secrets` | `vault_put` | Escribe un secreto en el vault del hive. |

### 9.2 Nodos por hive
| HTTP | Action |
|------|--------|
| `GET /hives/{id}/nodes` | `list_nodes` |
| `POST /hives/{id}/nodes` | `run_node` (`node_name`, `runtime`, `runtime_version`, `tenant_id`) |
| `DELETE /hives/{id}/nodes/{name}` | `kill_node` (`force`, `purge_instance`) |
| `GET /hives/{id}/nodes/{name}/status` | `get_node_status` |
| `GET/PUT /hives/{id}/nodes/{name}/config` | `get/set_node_config` |

Runbook de status por nodo (campos de `payload.node_status`):
`lifecycle_state` (`STARTING|RUNNING|STOPPING|STOPPED|FAILED|UNKNOWN`), `health_state`
(`HEALTHY|DEGRADED|ERROR|UNKNOWN`), `health_source` (`NODE_REPORTED|ORCHESTRATOR_INFERRED|UNKNOWN`),
`status_version` (monotónico, persiste restart del orchestrator).

- `FAILED` + `UNKNOWN` → revisar el unit y logs del nodo.
- `RUNNING` + `ORCHESTRATOR_INFERRED` → vivo pero no respondió el handler de status en timeout.

---

## 10. Layout de directorios (fuente: install.sh / postinst)

```
/etc/fluxbee/
├── hive.yaml                 # ÚNICO archivo que edita el operador
├── hive.yaml.example         # template (conffile del .deb)
├── io-cloud.env / io-blob.env(.example)

/var/lib/fluxbee/
├── ssh/motherbee.key(.pub)   # 0700 dir; llave de bootstrap ed25519
├── tls/<hive_id>             # CA de malla + cert (wan.mtls)
├── state/{nodes,cookbook,sy-admin,sy-edge,io-blob}
├── hives/                    # repo de spokes (info por hive)
├── opa/{current,staged,backup}
├── wf-rules/  modules/  nats/
├── blob/{,public}            # public = canal one-way a ingress
├── syncthing/                # home gestionado del syncthing (user fluxbee)
└── dist/
    ├── core/{bin,manifest.json}    # binarios del core + hashes (sendonly→spokes)
    ├── runtimes/                   # io.api, ai.generic, wf.engine, io.slack, io.linkedhelper
    └── vendor/syncthing/           # binario + manifest del syncthing vendorizado

/run/fluxbee/routers/          # sockets del router (volátil)
/dev/shm/jsr-*                 # regiones SHM (router, config, lsa, identity, opa, memory)
```

---

## 11. Protecciones (no se pueden matar por API)

| Componente | Kill por API | Razón |
|------------|:------------:|-------|
| RT.gateway | ❌ | Router raíz del hive |
| SY.orchestrator | ❌ | Proceso raíz |
| SY.admin / SY.vault | ❌ | Sin ellos no hay control/secreto |
| SY.config.routes / SY.opa.rules / SY.identity / SY.policy | ❌ | Críticos para routing/policy/identidad |
| AI.* / WF.* / IO.* (instanciados) | ✅ | Nodos de aplicación |

---

## 12. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura | `01-arquitectura.md` |
| Protocolo | `02-protocolo.md` |
| Conectividad WAN | `05-conectividad.md` |
| Regiones SHM | `06-regiones.md` |
| Edge/ingress v6 | `docs/edge-ingress-spec-v6.md` |
| IO.cloud provisioning | `docs/io-cloud-spec-v1.md` |
