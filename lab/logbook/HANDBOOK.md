# HANDBOOK — desplegar fluxbee desde cero

> **Qué es esto.** Las recetas que **funcionaron end-to-end** en el primer despliegue de fluxbee en
> producción (Proxmox `192.168.8.207`, 2026-07-28 → 07-30). Cada paso de acá se ejecutó de verdad y
> se verificó; lo que no se validó está marcado como tal.
>
> **Documentos hermanos:** [`METHOD.md`](METHOD.md) (cómo se trabaja) · [`FINDINGS.md`](FINDINGS.md)
> (qué se descubrió, con evidencia) · [`PENDING-BUGS.md`](PENDING-BUGS.md) (qué hay que arreglar) ·
> las bitácoras diarias (`2026-07-*.md`) tienen el detalle crudo de cada paso.
>
> **Regla de portabilidad** (`METHOD.md` §3 regla 0c): el próximo prod puede ser bare-metal u otro
> hipervisor. Cada receta marca qué es **específico de Proxmox** y qué es **del producto**. Las
> secciones §1–§3 son del hipervisor; **de §4 en adelante es fluxbee puro** y sirve igual en un
> servidor físico.

---

## 0. El modelo mental, en una frase

**Solo motherbee se instala con el `.deb`.** Los demás nodos son **máquinas Linux limpias** a las que
`add_hive` toma por SSH, bootstrapea, configura y **cierra**. No se instala fluxbee en un spoke a
mano: si te encontrás haciéndolo, estás peleando contra el diseño.

```
   [ motherbee ]  <- .deb + fluxbee-firstboot        el unico con Postgres y con el vault
        │
        │  add_hive  (SSH: entra abierta, sale cerrada)
        ├──────────► [ worker  ]   trabajo generico
        ├──────────► [ ingress ]   puerta publica, TLS 443     (2 placas)
        └──────────► [ egress  ]   NAT de salida a internet    (2 placas)
```

---

## 1. Decisiones que hay que tomar ANTES de tocar nada

No son opcionales: la mitad no se puede cambiar cómodamente después.

| Decisión | Lo que elegimos | Por qué |
|---|---|---|
| Red interna del mesh | `10.10.10.0/24`, sin gateway | Directamente conectada. Deja lugar a `10.10.11.x` etc. |
| ¿Quién da internet? | El **egress**, con NAT | Es su rol. Ver §7 sobre la trampa del gateway. |
| IP del ingress | Pública, propia | Ahí apunta el DNS. **No** puede estar detrás del NAT del egress. |
| Default route de cada nodo | Por su **pata externa** si la tiene | Una VM tiene **un solo** default gateway. La interna queda sin `gw`. |
| Hostname público | `hive-<hash>.dominio` | Ver §8: opaco a propósito. |
| Credencial de bootstrap | Un usuario con `NOPASSWD:ALL` | Requisito duro de `add_hive`. Se revoca sola al final. |

**Direccionamiento que usamos** (referencia concreta):

| VM | RAM/cores/disco | interna | segunda placa |
|---|---|---|---|
| `fb-mb` | 10G / 4 / 60G | `10.10.10.10` | — |
| `fb-worker1` | 4G / 2 / 30G | `10.10.10.20` | — |
| `fb-ingress` | 2G / 2 / 20G | `10.10.10.30` | pública `x.x.x.80/24` |
| `fb-egress` | 2G / 2 / 20G | `10.10.10.40` | admin `192.168.8.240/24` |
| `fb-build` | 8G / 4 / 80G | `10.10.10.50` | — *(fuera del cluster)* |

> **La VM de build va FUERA del cluster, a propósito.** El plan ante una falla de integración es
> *borrar el cluster y empezar de nuevo*. Si el `.deb` y el repo apt viven adentro, cada wipe cuesta
> una hora de compilación. Afuera, sobrevive.

> **Sumá la RAM antes, no después.** Planifiqué 18 G para el cluster y después agregué la build de
> 8 G sin re-sumar: 27.9 G asignados sobre 23 G físicos. KVM asigna a demanda y no dolió, pero es
> riesgo latente. Política: **la build queda apagada salvo cuando compila.**

---

## 2. Red del hipervisor · *específico de Proxmox*

Tres redes, y la interna es la única que fluxbee necesita:

```text
vmbr0   nic0   192.168.8.0/24     administracion (compartida, sin internet)
fbint    —     10.10.10.0/24      INTERNA fluxbee — SDN zona simple + SNAT
vmbr1   nic1   x.x.x.0/24         PUBLICA (el host NO tiene IP en ella)
```

**La interna se hace con SDN**, no con un bridge suelto:

```
Datacenter > SDN > Zones  -> tipo "Simple", ID: fluxint
Datacenter > SDN > VNets  -> ID: fbint, zona: fluxint
  subnet: 10.10.10.0/24, gateway: 10.10.10.1, SNAT: SI
Datacenter > SDN         -> Apply
```

Con `SNAT: sí`, el host rutea y enmascara: **las VMs internas salen a internet por la IP del host**
sin tocar nada más. Eso es lo que permite construir el template y compilar antes de que exista el
egress.

⚠️ **Una zona "Simple" es local al nodo.** No cruza hosts de un cluster Proxmox. Para multi-host hace
falta VXLAN o EVPN.

### 🔥 La trampa más cara de todas — Proxmox anidado sobre VMware

Si el Proxmox corre como VM de ESXi/vSphere, **el port group tiene que permitir**:

```
Promiscuous mode  : Accept
MAC address changes: Accept
Forged transmits  : Accept      <-- este es el que rompe todo
```

Con `Forged transmits: Reject`, vSphere descarta cualquier frame cuyo MAC origen no sea el de la VM
Proxmox. Las VMs anidadas **no pueden ni siquiera hacer ARP**. El síntoma es desconcertante porque
**la red interna funciona perfecto** (sale por el SNAT del host, con el MAC del host) mientras
cualquier placa bridgeada está muerta.

**Y hay que aplicarlo a CADA port group**, uno por placa física. Nosotros lo aplicamos al de `nic0`,
el egress arrancó a andar, y el ingress siguió roto exactamente igual — faltaba el de `nic1`.

> **Cómo se diagnostica** (sirve para cualquier "la red anda a medias"): buscá la **asimetría**. Si
> el tráfico que sale ruteado funciona y el bridgeado no, el problema no es de la VM ni del bridge:
> es **filtrado por MAC aguas arriba**.

---

## 3. Template base · *específico de Proxmox, pero el concepto es universal*

Objetivo: una imagen Linux limpia, con guest-agent y SSH, que se clone en máquinas **independientes**.

### 3.1 La imagen

```bash
POST /nodes/pve/storage/local/download-url
     content=import
     filename=noble-server-cloudimg-amd64.qcow2      # <-- .qcow2, NO .img
     url=https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img
```

⚠️ La imagen de Ubuntu **es qcow2 aunque se publique con extensión `.img`**. Con `.img` el endpoint
la rechaza (`invalid filename or wrong extension`). Renombrala al descargar.

### 3.2 El snippet de cloud-init — **lo tiene que escribir un humano**

La cloud image **no trae `qemu-guest-agent`**, y el guest-agent es el canal de acceso. Hay que
inyectarlo en el primer boot vía user-data… y **la API de Proxmox no permite depositar snippets**:
`upload` y `download-url` aceptan `iso, vztmpl, import` — `snippets` no está en la enumeración,
aunque el storage sí acepte el tipo.

El operador lo escribe **una vez** en el shell del host, y queda para siempre:

```yaml
# /var/lib/vz/snippets/tpl-userdata.yaml
#cloud-config
packages: [qemu-guest-agent, openssh-server, sudo]
ssh_pwauth: true
runcmd:
  - systemctl enable --now qemu-guest-agent
```

> **Nota de método.** Esto es un límite real, y **la respuesta correcta no es pedir SSH al
> hipervisor**. Un archivo puntual escrito por el operador es más barato que un shell root fuera del
> registro de tasks. Ver `METHOD.md` §2.

### 3.3 Crear, generalizar, convertir

```bash
POST /nodes/pve/qemu
     vmid=9000 name=fb-template cores=2 memory=2048
     scsi0=local-lvm:0,import-from=local:import/noble-server-cloudimg-amd64.qcow2
     ide2=local-lvm:cloudinit  agent=enabled=1  cpu=host
     net0=virtio,bridge=fbint
     ipconfig0=ip=10.10.10.9/24,gw=10.10.10.1  nameserver=1.1.1.1
     ciuser=fluxops  cicustom=user=local:snippets/tpl-userdata.yaml
     serial0=socket vga=serial0
# esperar a que la task TERMINE, despues:
PUT  .../resize  disk=scsi0 size=20G
POST .../status/start
```

⚠️ **Encadená por estado de task, nunca por `sleep`.** El `create` importa 624 MB y sostiene el lock
del VM todo ese tiempo; `resize` y `start` lanzados detrás fallan con
`can't lock file '/var/lock/qemu-server/lock-9000.conf'`. Poleá `/tasks/<UPID>/status` hasta
`stopped OK`.

**Generalizar** (adentro de la VM, antes de convertir) — **esto es lo que hace que los clones sean
máquinas distintas**:

```bash
cloud-init clean --logs --seed          # cada clon re-ejecuta cloud-init con SU config
truncate -s 0 /etc/machine-id           # si no, todos los clones piden el MISMO lease DHCP
ln -sf /etc/machine-id /var/lib/dbus/machine-id
rm -f /etc/ssh/ssh_host_*               # si no, comparten identidad SSH
apt-get clean; rm -rf /var/lib/apt/lists/*
journalctl --rotate; journalctl --vacuum-time=1s
```

**Verificá antes de convertir:** `machine-id` en 0 bytes · 0 host keys · cloud-init `not started`.

```bash
POST .../config  delete=cicustom,ipconfig0     # <-- ver la trampa de abajo
POST .../status/shutdown                        # limpio, no stop
POST .../qemu/9000/template
```

### ⚠️ `cicustom` y `ciuser` no conviven

Con `cicustom=user=...`, Proxmox usa **ese archivo EN LUGAR del user-data que genera** — y ese es
justo el que crea el usuario. `ciuser`/`cipassword` quedan **sin efecto** y el usuario nunca existe.

**Por eso se borra `cicustom` del template**: el snippet solo hacía falta una vez, para hornear el
agente en la imagen. Ya con el agente adentro, los clones usan el cloud-init estándar y cada uno crea
su usuario normalmente. **Los clones no dependen del snippet.**

### 3.4 Validar el template con el primer clon

No des el template por bueno sin esto:

| Chequeo | Esperado |
|---|---|
| `machine-id` | **distinto** del template |
| host keys SSH | **3, regeneradas** |
| `qemu-guest-agent` | `active` |
| usuario de bootstrap | existe, con `NOPASSWD:ALL` |
| `PasswordAuthentication` | efectivo **`yes`** |

Sobre el último: en Ubuntu 24.04 conviven `50-cloud-init.conf` (`yes`) y `60-cloudimg-settings.conf`
(`no`). **Gana el primero por orden lexicográfico.** Verificalo de verdad, no de la lectura:

```bash
ssh -o BatchMode=yes -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    fluxops@127.0.0.1 true
# "Permission denied (publickey,password)" => el servidor OFRECE password. Correcto.
```

### ⚠️ Un clon recién booteado NO está listo

La cloud image de Ubuntu dispara `unattended-upgrades` en el primer boot: baja ~15 MB, escribe
~2.5 GB, **reinicia sola**, y el guest-agent puede no volver (se está actualizando a sí mismo).
Observado en cada clon, sin excepción.

**Receta:** después de clonar, esperá a que termine el ciclo y **reiniciá la VM** antes de
considerarla operativa. Confirmá con: cloud-init `done`, agente `active`, `dpkg --audit` limpio, sin
`/var/run/reboot-required`.

Esto importa **directamente** para `add_hive`, que tiene un timeout de 180 s.

### 3.5 Operar por guest-agent — lo que hay que saber antes, no después

Si trabajás contra las VMs por la API de Proxmox (`agent/exec`), esto te va a pasar:

**El agente se muere bajo carga de I/O.** Un upgrade del `.deb`, un arranque en frío, una
compilación: el `qemu-guest-agent` deja de responder y la API contesta
`QEMU guest agent is not running` con la VM **perfectamente viva**. No es la VM: es el agente.

```bash
# 1. antes de asustarte, confirmá que la VM esta viva DESDE OTRA VM tuya
#    (misma red interna; no hace falta el agente)
ping -c2 10.10.10.10
for p in 9000 9100; do timeout 3 bash -c "echo > /dev/tcp/10.10.10.10/$p" && echo "$p ok"; done

# 2. recuperar el agente: reboot ACPI primero
POST /nodes/pve/qemu/<id>/status/reboot
# vuelve en ~70s... y si la VM sigue con carga se puede volver a morir.
# 3. si se muere de nuevo: reset duro. Ahi aguanta.
POST /nodes/pve/qemu/<id>/status/reset
```

> Un reboot/reset de motherbee **no es gratis pero tampoco es grave**: los 4 hives vuelven solos.
> Es, de hecho, la prueba más dura del arranque en frío. Pero decidilo a propósito, no por pánico.

**Todo lo que dure más de unos segundos va con `setsid nohup` a un log.** Si el agente se muere a
mitad, el comando sobrevive y leés el log después. Sin esto, perdés el trabajo Y no sabés si corrió:

```bash
setsid nohup /root/loquesea.sh > /root/loquesea.log 2>&1 < /dev/null &
```

Corolario que aprendí a la mala: si el agente muere durante la llamada que **crea** el script, el
script nunca existió. Antes de asumir que corrió y quedó a mitad, mirá si el log está.

**Secretos por stdin, nunca por argv.** `agent/exec` acepta `input-data` (base64). Todo lo que pase
por `command=` queda en el **log de tasks de Proxmox**:

```bash
# lab/qga_stdin.sh — pasa el payload por stdin, no por la linea de comandos
POST agent/exec  command=/bin/bash  command=-lc  command="<script que lee stdin>"  input-data=<b64>
```

### ⚠️ Después de generalizar, el primer `apt` necesita `update` COMPLETO

La generalización borra `/var/lib/apt/lists/*`. Si hacés un `apt update` filtrado a un solo repo (por
ejemplo el de fluxbee), las listas de Ubuntu no están y la instalación falla con
`Depends: postgresql but it is not installable`. **Un `apt-get update` completo** (≈39 MB) lo
resuelve.

---

## 4. Construir el `.deb` — *fluxbee puro*

En una VM aparte, con el toolchain que fija el repo:

```bash
# rustc/cargo (la version EXACTA del repo), go, protobuf-compiler,
# build-essential, pkg-config, libssl-dev, git, dpkg-dev, apt-utils
git clone git@github.com:<org>/fluxbee.git /opt/fluxbee
bash packaging/build-deb.sh 0.1.0
```

**Rendimiento medido:** Rust release **~55 min** con 4 vCPU en un Xeon E5-2620 @2.0 GHz. Ciclo
completo con Rust cacheado: ~27 min. **Corrélo con `nohup`**: sobrevive a que se caiga tu sesión.

### ⚠️ Cloná y compilá con el MISMO usuario

Si clonás como un usuario y compilás como `root`, git marca *dubious ownership* y el paso de Go muere
con `error obtaining VCS status: exit status 128` — **después de que Rust ya compiló 55 minutos**.

```bash
git config --global --add safe.directory /opt/fluxbee
```

> El propio error sugiere `-buildvcs=false`. **No lo uses**: apaga el estampado de versión en los
> binarios para tapar un problema de permisos. Arreglá la causa.

### El toolchain vive en el `HOME` de quien compila

`rustup` busca su metadata en `$HOME/.rustup`. Si ejecutás por guest-agent con un `HOME` que no es
el del dueño del toolchain, te dice **"no installed toolchains"** con el directorio de la toolchain
ahí mismo, presente y completo:

```
error: rustup could not choose a version of cargo to run, because one wasn't
       specified explicitly, and no default is configured
```

No es que falte Rust: es que estás mirando el `.rustup` equivocado. En `fb-build` el toolchain es de
`root`, así que el script de build empieza con `export HOME=/root` — y el `git fetch`, en cambio, va
como `fluxops`, que es quien tiene la deploy key. Los dos usuarios son necesarios, cada uno en su
paso.

> Y si ya corriste git como `root` por error, devolvé los objetos antes de seguir, o el próximo
> fetch como `fluxops` muere con `insufficient permission`:
> `find /opt/fluxbee/.git -user root -exec chown fluxops:fluxops {} +`

### Verificá el `.deb` antes de confiar en él

```bash
ls -la dist/fluxbee_*.deb              # ~240 MB. Si pesa 2 KB, salio truncado.
dpkg-deb -c dist/fluxbee_*.deb | wc -l # ~111 entradas
dpkg-deb -c dist/fluxbee_*.deb | grep -E "ai_node_runner|wf-generic|io-"
```

### Publicarlo como repo apt

```bash
scripts/apt-repo-publish.sh --serve     # sirve en :8900 (unit fluxbee-apt)
```

**Instalá siempre por `apt`, nunca con `dpkg -i`** — el paquete depende de `postgresql` y `apt` es el
camino documentado que resuelve dependencias.

#### ⚠️ Usá el script. No rehagas el índice a mano.

El script hace **tres** cosas en una línea, y las tres importan:

```bash
dpkg-scanpackages -m . > Packages  &&  gzip -kf Packages  &&  apt-ftparchive release . > Release
```

Copiar un `.deb` nuevo a la carpeta **no alcanza**. Si lo hacés a mano, hay dos maneras de que
salga mal y ninguna avisa:

| omisión | síntoma | por qué engaña |
|---|---|---|
| no regenerar `Release` | `apt` dice **`fluxbee is already the newest version`** con el paquete nuevo ahí | `apt` ve `Hit:` en `Release` y ni mira `Packages` |
| `dpkg-scanpackages` **sin `-m`** | el índice queda con **una sola** versión | sin `-m` publica sólo la más nueva por paquete, y te quedás **sin rollback por apt** |

La segunda es la peligrosa: no rompe nada hoy, te saca el camino de vuelta. Se nota comparando el
tamaño de `Packages` — 13 versiones son ~13 KB, una sola son ~1 KB.

> Esto está escrito porque lo pisé: hice el índice a mano, me comí las dos, y perdí dos ciclos.
> El script existía desde el principio.

#### ⚠️ `dpkg-deb` parece colgado y no lo está

El último paso comprime con `xz`. Durante **varios minutos** el `.deb` se queda en ~3 KB, y si lo
mirás en ese momento `dpkg-deb -c` te dice `unexpected end of file in archive member header`.

No está truncado: está comprimiendo. Confirmalo antes de alarmarte:

```bash
ps aux | grep dpkg-deb        # 300-400% de CPU = está trabajando
```

El paquete terminado pesa **~240 MB**. La línea `=== BUILD END rc=0 ===` es la única señal de que
terminó; el tamaño del archivo a mitad de camino no dice nada.

---

## 5. Motherbee

```bash
# en la VM de motherbee
echo "deb [trusted=yes] http://10.10.10.50:8900 ./" > /etc/apt/sources.list.d/fluxbee.list
apt-get update            # COMPLETO (ver 3.4)
apt-get install -y fluxbee
fluxbee-firstboot
```

`fluxbee-firstboot` es **idempotente-ish pero irreversible en la práctica**: bootstrapea el hive.
Su log cuenta exactamente qué hace:

```text
[1/5] postgresql + DB bootstrap
[2/5] start orchestrator          (crash-loopea hasta que aterriza el secreto — es normal)
[3/5] wait admin + vault_put postgres secret
[4/5] reconnect DB consumers
[5/6] wait hive ready + start motherbee singleton nodes
[6/6] spawn base node instances   (arrancan degradados, sin configurar)
```

**Aceptación:**

```bash
curl -s http://127.0.0.1:8080/hives                      # motherbee: alive
systemctl list-units 'sy-*' 'fluxbee*' 'rt-*' --all      # 19 units active running
systemctl --failed                                        # vacio
curl -s http://127.0.0.1:8080/hives/motherbee/commands   # el audit log ya registra
```

Los nodos base (`IO.api`, `IO.slack`, `IO.wapp.default`, `AI.chat`) arrancan en `UNCONFIGURED`.
**Eso es correcto**, no es una falla: el arranque degradado es de diseño.

> **`wan.authorized_hives` vacío = permite todo.** No está en el `hive.yaml` que genera el postinst y
> parece que falta. No falta: `add_hive` solo aplica el allowlist si **no** está vacío, y el propio
> mensaje de error del código sugiere *"leave authorized_hives empty"*. Sin acción.

---

## 6. Los spokes — `add_hive`

**Un comando contra una máquina Linux vacía.** Entra abierta, sale cerrada.

```jsonc
POST /hives                          // accion: add_hive  (NO /hives/<hive>/hives)
{
  "hive_id": "worker1",
  "role": "worker",                  // worker | ingress | egress
  "address": "10.10.10.20",
  "ssh_user": "fluxops",
  "ssh_password": "<credencial del template>",   // o ssh_key
  "harden_ssh": true                 // <-- NO ES EL DEFAULT. Ponelo siempre.
}
```

### ⚠️ `harden_ssh` viene en `false`

El endurecimiento funciona perfecto cuando se lo pide (lo verificamos nodo por nodo). Pero hay que
**acordarse**. Un default inseguro en el camino feliz → [PB-6](PENDING-BUGS.md#pb-6).

### Qué verificar después de cada join — **no te quedes con lo que reporta**

```bash
sshd -T | grep -i passwordauth        # no
wc -l < ~fluxops/.ssh/authorized_keys # 0  (la clave de MB se revoco)
ls /etc/sudoers.d/                    # sin fluxbee-orchestrator
systemctl list-units 'fluxbee*' 'rt-*' --no-legend | wc -l   # 9 servicios
```

### egress — necesita más

```jsonc
{ "role": "egress", "lan_cidr": "10.10.10.0/24",
  "lan_iface": "eth0", "wan_iface": "eth1" }
```

**`nftables` tiene que estar instalado antes** — `add_hive` egress **falla cerrado** sin él.

Verificá el NAT **en la caja**, no en la respuesta:

```bash
nft list ruleset | grep -E "masquerade|10.10.10"
sysctl net.ipv4.ip_forward                       # 1
journalctl -u <unidad del egress> | grep nat_applied
```

> **`internet_reachable` no llega a motherbee.** Se verifica solo en el host egress y no se transmite
> (v1, T-VER-1): la respuesta de `add_hive` lo devuelve `null`. Mirá el journal del egress.

### ingress — el certificado va ANTES del join

**Sembrá el TLS en el vault primero**, o el edge levanta sin HTTPS:

```jsonc
POST /hives/motherbee/vault/secrets
{ "key": "edge_tls_<dominio>",
  "value": { "cert": "<PEM: leaf + intermedios>", "key": "<PEM privada>" },
  "metadata": { "resource_type": "tls", "owner_node": "SY.edge@ingress1" } }
```

Después:

```jsonc
{ "role": "ingress", "ingress": { "listen": "0.0.0.0:443" } }
```

`allow_plaintext` está **prohibido** en 443. El edge es *fail-closed*: si el TLS no se puede armar,
**no** cae a texto plano — no levanta el frontend público.

### ⚠️ Las placas secundarias las configurás vos

`add_hive` **no tiene comandos para configurar placas de red secundarias**, pero los roles `ingress`
y `egress` **requieren** una segunda placa por definición. Hay que dejarlas configuradas antes y
pasarle los nombres de interfaz. Rompe la promesa de "una máquina vacía" justo en los dos roles que
más la necesitan → [PB-5](PENDING-BUGS.md#pb-5).

### ⚠️ DNS: no le pongas el mismo nameserver a todos

Puse `nameserver=192.168.8.1` (la red de admin) a las cuatro VMs por igual. El **ingress no tiene
ruta a esa red** → se quedó sin DNS. **Los nodos con pata pública necesitan un DNS público**
(`1.1.1.1`).

---

## 7. La trampa del gateway — `egress.gateway_ip`

El egress hace NAT, pero **los nodos tienen que usarlo como gateway** para que sirva de algo. Eso se
declara en el `hive.yaml`.

⚠️ **El campo es, por diseño, la ruta de los WORKERS.** Motherbee **no entra en el modelo de
egress** — y no es un olvido: Fluxbee nunca reescribe la ruta por defecto del propio plano de
control (hacerlo desde un campo de yaml, en cada boot y cada tick de watchdog, sin rollback, en la
caja que provisiona el egress, es un lockout). → [PB-8](PENDING-BUGS.md#pb-8).

Pero motherbee **es** donde corren los nodos que llaman a internet (`IO.slack.*`, `IO.wapp.*`,
`AI.*`), así que el orchestrator **verifica** su ruta contra `gateway_ip` y loguea
`EGRESS_MOTHERBEE_BYPASS` cuando no coinciden. Es observación, no corrección: si querés a motherbee
sobre el egress, ponelo a nivel SO/red.

⚠️ **Y la trampa más inmediata: hoy puede no rutear NADIE por el egress.** La inyección solo alcanza
a los workers provisionados **después** de declarar el campo — un worker que se unió antes sigue con
su gateway original.

**No des por hecho que tu motherbee sale por el egress** sin comprobarlo:

```bash
curl -s https://ifconfig.me      # desde motherbee: sale por la IP del egress?
ip route get 1.1.1.1
```

---

## 8. DNS y certificado

**Registro A**: `hive-<hash>.<dominio>` → IP pública del ingress.

Elegimos un **hash opaco de un solo label** (`hive-k3m9x7q2`), no `ingress1` ni el nombre del
cliente, por tres razones:

1. El wildcard `*.dominio` **solo cubre un label** — `a.b.dominio` no entra.
2. Ese hostname **va a terminar escrito en configuraciones ajenas** (webhooks de Meta, Slack…).
   Cuanto menos diga, mejor.
3. No filtra ni el cliente ni cuántos backends hay.

Es **deliberadamente distinto del `hive_id` interno**: la puerta externa pertenece al edge, no al
hive (decisión D7).

### El certificado: mandá la cadena completa

**Verificá el archivo antes de cargarlo** — el nuestro venía con el `-----END CERTIFICATE-----` del
leaf pegado al `-----BEGIN` del intermedio en el mismo renglón, y así **openssl lee cero
certificados**:

```bash
grep -c "BEGIN CERTIFICATE" cadena.crt
openssl crl2pkcs7 -nocrl -certfile cadena.crt | openssl pkcs7 -print_certs -noout
openssl verify -untrusted intermedios.pem leaf.pem
# la key coincide?
diff <(openssl x509 -in leaf.pem -noout -modulus) <(openssl rsa -in privada.key -noout -modulus)
```

Serví **leaf + intermedios**, sin la raíz self-signed (ya está en los trust stores; solo suma bytes
por handshake).

> **Por qué importa mandar la cadena.** Sin el intermedio, un navegador o `curl` con la CA cacheada
> **igual da OK** (lo busca por AIA). Un cliente estricto —Go, Python, los webhooks de Meta y
> Slack— falla. **Se ve bien justo donde no importa.**

**Probalo con un cliente que NO haga fetch AIA:**

```python
import ssl, socket
c = ssl.create_default_context()
with socket.create_connection(("hive-xxx.dominio", 443), timeout=20) as s:
    with c.wrap_socket(s, server_hostname="hive-xxx.dominio") as t:
        print(t.version(), t.getpeercert()['notAfter'])
```

### ⚠️ Rotar el cert exige reiniciar `sy-edge`

El material TLS se lee **una sola vez al boot**. Cargarlo en el vault **no** alcanza: hay que
reiniciar el servicio. Mitigado en el código (el edge ahora sale con `exit(0)` y systemd lo
reinicia), pero **verificá que tu binario lo tenga** → [PB-1](PENDING-BUGS.md#pb-1).

---

## 9. Aceptación final

```bash
# 1. topologia completa
curl -s http://127.0.0.1:8080/hives
#    motherbee alive · worker1/ingress1/egress1 connected

# 2. HTTPS publico, desde AFUERA del datacenter
curl -sS -o /dev/null -w "%{http_code} verify=%{ssl_verify_result}\n" https://hive-xxx.dominio/
#    404 verify=0   <-- el 404 es CORRECTO: SY.edge solo sirve /e/<ich>

# 3. cadena completa
echo | openssl s_client -connect hive-xxx.dominio:443 -servername hive-xxx.dominio -showcerts 2>/dev/null \
  | grep -c "BEGIN CERTIFICATE"      # >= 2

# 4. el egress rutea de verdad
#    (desde un worker) curl -s https://ifconfig.me  -> IP del egress
```

**Reboot del hipervisor:** validado. Las 4 VMs arrancaron en frío a la vez y **el mesh se rearmó
solo**, sin intervención. Dos advertencias:

- **Esperá la convergencia antes de diagnosticar.** Los primeros 30 s muestran servicios inactivos y
  `NODE_NOT_FOUND`. No es una falla: es el arranque.
- `sy-edge` puede llegar a `restart counter is at 9` antes de estabilizar → [PB-4](PENDING-BUGS.md#pb-4).

---

## 10. Tabla de trampas — el resumen que conviene leer antes de empezar

| # | Trampa | Síntoma | Solución |
|---|---|---|---|
| 1 | Port security de VMware | Las VMs anidadas **no hacen ni ARP**; la interna anda igual | `Forged transmits: Accept` **en cada** port group |
| 2 | Lock de VM en Proxmox | `can't lock file ... got timeout` | Encadenar por estado de task, no por `sleep` |
| 3 | Cloud image `.img` | `invalid filename or wrong extension` | Renombrar a `.qcow2` |
| 4 | API sin snippets | `snippets` no está en la enumeración | Lo escribe el operador, una vez |
| 5 | `cicustom` vs `ciuser` | El usuario nunca se crea | O uno o el otro; borrar `cicustom` del template |
| 6 | Clon recién booteado | Reinicia solo, el agente desaparece | Esperar el ciclo + **reiniciar** |
| 7 | `apt update` filtrado | `Depends: postgresql but it is not installable` | `apt-get update` **completo** |
| 8 | Clonar y compilar con usuarios distintos | `VCS status: exit status 128` tras 55 min | `git config --global --add safe.directory` |
| 9 | `.deb` truncado | `dpkg-deb` sale 0 con 2 KB de paquete | Verificar tamaño y entradas (ya hay preflight) |
| 10 | `harden_ssh` en `false` | La caja queda abierta | Pasarlo `true` explícito |
| 11 | `nftables` ausente | `add_hive` egress falla cerrado | Instalarlo antes |
| 12 | DNS uniforme | El ingress se queda sin resolución | DNS público en los nodos con pata pública |
| 13 | Cadena TLS incompleta | Anda en el navegador, falla en los webhooks | Servir leaf + intermedios; probar sin AIA |
| 14 | Cert rotado en el vault | El edge sigue sirviendo el viejo | Reiniciar `sy-edge` (ver PB-1) |
| 15 | Placas secundarias | `add_hive` no las configura | Dejarlas listas antes del join |
| 16 | Repo apt hecho a mano | `apt` dice *already the newest version* con el `.deb` nuevo ahí | Usar `scripts/apt-repo-publish.sh`: regenera **`Release`**, no sólo `Packages` |
| 17 | `dpkg-scanpackages` sin `-m` | El índice queda con una sola versión · **te quedás sin rollback** | El script ya trae `-m`. No lo rehagas a mano |
| 18 | `dpkg-deb` "colgado" | El `.deb` sigue en 3 KB y `dpkg-deb -c` dice *unexpected end of file* | Está comprimiendo con `xz`. Esperá el `BUILD END`; termina en ~240 MB |
| 19 | Toolchain y `$HOME` | `rustup` dice *no installed toolchains* con la toolchain ahí | `rustup` lee `$HOME/.rustup`: en `fb-build` compilá con `HOME=/root` |
| 20 | Guest-agent muerto | `QEMU guest agent is not running` con la VM viva | Sondear desde otra VM tuya; recuperar con reboot y si no, `reset` |
| 21 | Comando largo por agente | El agente muere a mitad y perdés el trabajo | `setsid nohup ... > log` y leer el log |
| 22 | Puerto/unit equivocados | "se cayó el edge / el router / el admin" | `SY.edge` va en **:443**; en un spoke no hay `sy-router`; el admin es **loopback** |
| 23 | `payload` de la API | Un `while` que gira sobre una operación exitosa | El `status` de arriba es el del sobre; lo real está bajo `payload.` |
| 24 | `verify=19` en TLS local | "la cadena está rota" | Es falta de SNI. Probá con `--resolve` y un host que matchee el wildcard |
| 25 | Runtime hot-publicado | El `.deb` sube y ese nodo queda en la versión vieja, **sin aviso** | El seed respeta tu `current` a propósito. Promovelo con `publish-runtime.sh --set-current` |

---

## 11. Qué es portable y qué no

**Del producto — sirve igual en bare-metal:** §4 (build del `.deb`), §5 (motherbee), §6 (`add_hive`),
§7 (gateway), §8 (DNS/cert), §9 (aceptación). Trampas 8–15.

**Específico de Proxmox/VMware:** §2 (SDN, port groups), §3 (template, cloud-init, guest-agent).
Trampas 1–7.

**En bare-metal, §2 y §3 se reemplazan por:** cablear/VLANear las tres redes, e instalar Ubuntu Server
en cada máquina con un usuario `NOPASSWD:ALL` y SSH por password habilitado. **`add_hive` no cambia
en absoluto** — su contrato es "una máquina Linux limpia y alcanzable por SSH", y de dónde salió esa
máquina le da igual.

---

## 12. Actualizar un despliegue existente

**Validado el 2026-07-30** con cinco upgrades encadenados en producción. Los tres bloqueantes que
la auditoría previa había encontrado están cerrados; detalle en [`PENDING-BUGS.md`](PENDING-BUGS.md).

### El ciclo

```bash
# 1. en la build box
git pull && bash packaging/build-deb.sh 0.1.N
bash scripts/apt-repo-publish.sh --serve      # publica y CONSERVA las anteriores

# 2. en motherbee
apt-get update && apt-get install -y fluxbee  # nunca dpkg -i

# 3. a cada spoke (opcional; el .deb solo actualiza motherbee)
POST /hives/{spoke}/update  {"category":"core","manifest_version":0,"manifest_hash":"<el de motherbee>"}
#    el hash sale de: GET /hives/motherbee/versions  ->  payload.hive.core.manifest_hash
```

**Tiempo real:** ~6 min de build con la caché tibia, ~1 min el `apt install`, y el sistema se
acomoda solo en ~25 s.

### Cómo saber si el update fue REAL y no fantasma

No alcanza con el `status: ok`. El chequeo que distingue un update genuino de uno que no hizo nada:

```bash
p=$(systemctl show -p MainPID --value sy-orchestrator)
readlink /proc/$p/exe        # si dice "(deleted)" -> corre el binario VIEJO
systemctl show -p ActiveEnterTimestamp --value sy-orchestrator   # debe ser POSTERIOR al update
sha256sum /usr/bin/sy-orchestrator /var/lib/fluxbee/dist/core/bin/sy-orchestrator   # deben coincidir
```

### ⚠️ Un runtime publicado en caliente NO avanza con el `.deb`

El seed del paquete mueve `current` de cada runtime base a la versión nueva… **salvo el de un
runtime que vos hayas publicado en caliente y cuyo directorio siga en disco**. Ahí lo respeta y
publica la del paquete como *available* nada más:

```text
NOTE: base runtime io.api     'current' moved 0.1.13 -> 0.1.15 (package authority)
NOTE: base runtime ai.generic 'current' moved 0.1.13 -> 0.1.15 (package authority)
   ...y io.slack NO aparece en la lista
```

Es deliberado y correcto: si pisara tu `current`, cada upgrade te desharía el despliegue en
silencio. Pero el efecto colateral es igual de silencioso al revés — **ese nodo se queda atrás y
nada te avisa**. El paquete sube, todo lo demás avanza, y ese runtime sigue en la versión vieja.

Verificá siempre después de un upgrade:

```bash
python3 -c "
import json; m=json.load(open('/var/lib/fluxbee/dist/runtimes/manifest.json'))
for k,v in m['runtimes'].items(): print(k, v['current'], v['available'])"
```

Para aceptar la del paquete, promovela con la misma herramienta que usa el seed, y reiniciá el
nodo **por admin**:

```bash
sudo /usr/share/fluxbee/publish-runtime.sh --runtime io.slack --version 0.1.15 \
     --binary /var/lib/fluxbee/dist/runtimes/io.slack/0.1.15/bin/io-slack \
     --dist-root /var/lib/fluxbee/dist --set-current
curl -sS -X POST "$ADMIN/hives/$HIVE/nodes/$NODE_ENC/restart" -d '{"confirm":true}'
```

> El `restart` por admin **sí** te lleva a `current` — pero recién desde 0.1.15. Antes resolvía la
> última versión concreta guardada en vez de la intención (`requested_runtime_version: "current"`),
> y encima reiniciaba la unit sin regenerarla: reportaba la versión nueva **corriendo el binario
> viejo**.

### ⚠️ Cómo NO leer una falla donde no la hay

Tres veces en una jornada leí un sistema sano como roto. Las tres son de sondeo, no de producto —
y las tres se evitan sabiendo dónde mira cada cosa:

| lo que probé | lo que vi | lo que pasaba de verdad |
|---|---|---|
| `curl https://127.0.0.1:8443/` en el ingress | conexión rechazada, "el edge se cayó" | **`SY.edge` escucha en `:443`.** El `8443` es de otra época |
| `systemctl is-active sy-router` en un worker | `inactive`, "el worker perdió el router" | **En un spoke esa unit no existe.** Listá con `systemctl list-units "sy-*"` antes de preguntar por una |
| `curl :8080/hives` desde el worker | sin respuesta, "el admin murió" | **La API de admin escucha en loopback.** Se consulta desde motherbee, no desde un spoke |

Y una cuarta, de TLS: contra `127.0.0.1` vas a ver `ssl_verify_result=19`
(*self-signed in chain*) aunque la cadena esté perfecta — es que no hay SNI ni hostname que
validar. Verificá con un nombre concreto que matchee el wildcard del cert:

```bash
# el CN es *.fluxbee.ai — un literal "*.fluxbee.ai" NO sirve como host
H=hive-test.fluxbee.ai
curl -s -o /dev/null -w "http=%{http_code} verify=%{ssl_verify_result} certs=%{num_certs}\n" \
     "https://$H/" --resolve "$H:443:127.0.0.1"
# esperado: http=404 verify=0 certs=4
```

### ⚠️ La respuesta de la API vive bajo `payload`

Todo lo que devuelve la API de admin viene envuelto:

```json
{"action":"...", "status":"...", "error_code":null, "payload":{ ...lo que te importa... }}
```

El `status` de arriba es el del **sobre**, y dice `ok` aunque la operación de adentro haya fallado.
Rutas reales que hacen falta seguido:

```text
GET /hives                     ->  payload.hives[]           (NO payload.hives[].status para el join: es payload.hives[].join.phase)
GET /hives/<h>/versions        ->  payload.hive.core.manifest_hash
GET /versions                  ->  payload.comparison.verdict   y   payload.comparison.hives[]
```

> Esto ya causó un bug real: cuatro loops de e2e leían `payload.status` —siempre `"ok"`— y habrían
> girado 600 s sobre un join **exitoso**. Si estás por escribir un `while` sobre una respuesta de
> esta API, chequeá primero en qué campo estás mirando.

### ⚠️ `update category=core` devuelve TIMEOUT aunque funcione

La llamada corta a los 60 s con `error_code: TIMEOUT` **incluso cuando el update se completó**: el
update reinicia al propio `sy-orchestrator` del destino, que es justamente quien debía responder.
**No lo reintentes ciegamente** — verificá con el chequeo de arriba antes de concluir nada.

### ⚠️ Los nodos runtime y el orden de recuperación

Un upgrade reemplaza `dist/runtimes/<rt>/<version>/` y borra la anterior. Los nodos que siguen a
`current` se **rebindean y reinician solos** al arrancar el orquestador nuevo (buscá
`runtime 'current' pointer moved` en el journal). Si alguno queda colgado, la recuperación manual
tiene un **orden obligatorio**:

```bash
# UNA sola llamada: baja el nodo Y borra su directorio persistido.
DELETE /hives/{h}/nodes/{n}@{h}   -d '{"purge_instance":true}'

POST   /hives/{h}/nodes                 # run_node — tenant_id es OBLIGATORIO
  {"node_name":"IO.api","runtime":"io.api","runtime_version":"current",
   "tenant_id":"tnt:00000000-0000-0000-0000-000000000001"}
```

⚠️ **Usá `purge_instance`, no dos llamadas.** Encadenar `kill_node` y después
`remove_node_instance` es una **carrera**: el kill responde `ok` antes de que systemd baje la unit
y el remove falla con `NODE_INSTANCE_RUNNING`. Y si eso pasa sin que nadie mire la respuesta, el
config **queda** y el nodo **reaparece solo** en el próximo arranque, porque la reconciliación lo
relanza desde el config persistido. El nombre va **calificado** (`<node>@<hive>`).

⚠️ **Esta recuperación pierde la configuración del nodo.** Con nodos `UNCONFIGURED` no cuesta nada;
con tokens cargados es una pérdida real.

### El rollback

No existe rollback de core como comando ([U-5](PENDING-BUGS.md#u-5)). Los dos caminos reales:

1. **Snapshot en frío de las VMs** — ver §1 sobre por qué en frío.
2. **Conservar el `.deb` anterior publicado** en el repo apt y bajar de versión con `apt`.

---

## 13. Lo que este handbook todavía NO cubre

Honestidad sobre el alcance — nada de esto se probó:
- **Publicar y actualizar paquetes de nodo en caliente** (`publish_runtime_package`), distinto del
  `.deb`. Solo se probó el camino del `.deb`.
- Segundo hive de trabajo, o multi-hive sobre WAN en esta infraestructura.
- Backup y restore de motherbee (Postgres + vault).
- Renovación de certificado end-to-end.
- Recuperación ante la pérdida de motherbee.
