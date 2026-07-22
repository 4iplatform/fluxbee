# Integrated from-scratch deploy — findings (2026-07-22)

**Objetivo (user):** no que "funcione", sino **aprender qué cambiar para que quede bien y detectar bugs**.
Deploy completo desde cero en PC-004-157: motherbee (apt+firstboot) + worker + ingress (2 NICs) +
egress (NAT), sobre el repo apt interno (192.168.4.200). Cada fricción se registra como
**hallazgo → qué cambiar (template / firstboot / scripts / código)**.

Leyenda de destino del fix: `[TEMPLATE]` prep de la imagen base · `[FIRSTBOOT]` fluxbee-firstboot ·
`[DEPLOY]` add_hive/install.sh/scripts · `[CODE]` binarios/lógica · `[DOC]` documentación.

---

## F-1 `[TEMPLATE]` — machine-id horneado en el template ⇒ colisión de IP en todos los clones

**Síntoma:** los 4 clones del template 9000 arrancaron con la **misma IP `192.168.4.74`** por DHCP.

**Causa raíz:** el template `ubuntu-2404-fb` (9000) tiene `/etc/machine-id` **poblado** (no vacío) y
la red por cloud-init/DHCP. systemd-networkd/cloud-init derivan el **DHCP DUID/client-id del
machine-id**; como todos los clones comparten machine-id, el servidor DHCP les entrega el **mismo
lease** → colisión de IP (y potenciales colisiones de identidad aguas abajo).

**Qué cambiar `[TEMPLATE]`:** antes de convertir la VM en template, **vaciar** machine-id para que
cada clone regenere uno único en el primer boot (práctica estándar de cloud-template). Script:

```bash
# correr DENTRO de la VM base, luego apagar y convertir a template
truncate -s0 /etc/machine-id
rm -f /var/lib/dbus/machine-id && ln -s /etc/machine-id /var/lib/dbus/machine-id
cloud-init clean --logs --seed          # limpia estado de cloud-init (instance-id, net)
rm -f /etc/ssh/ssh_host_*               # regenera host keys únicas por clone (evita SSH host-key idénticas)
truncate -s0 ~/.bash_history 2>/dev/null || true
```

**Workaround aplicado en esta corrida (NO es el fix real):** regen manual de machine-id + IP
estática única por VM (`.210` MB, `.211` worker, `.212` ingress, `.213` egress) por encima del pool
DHCP (`.20–.150`). Sirve para seguir la prueba, pero el arreglo correcto es en el template.

**Bonus detectado:** las host keys SSH del template también se clonan idénticas → cuando el MB
haga el bootstrap SSH a los clean boxes, todos presentan la misma host key. El `rm ssh_host_*` de
arriba lo cubre. Verificar si el bootstrap de add_hive pinnea/verifica host keys (ver F-siguientes).

---

## F-2 `[TEMPLATE]` — estado dpkg interrumpido en la imagen base ⇒ apt-install roto en los clones

**Síntoma:** en el MB, `apt-get install fluxbee` abortó con
`E: dpkg was interrupted, you must manually run 'dpkg --configure -a'` (exit 100).

**Causa raíz:** el template quedó con una transacción dpkg a medio terminar (probablemente por un
apt/reboot durante la preparación de la imagen). Todo clone hereda ese estado sucio y no puede
instalar nada hasta reparar dpkg.

**Qué cambiar `[TEMPLATE]`:** dejar el árbol de paquetes limpio antes de templetizar:

```bash
dpkg --configure -a
apt-get -f install -y
apt-get clean && rm -rf /var/lib/apt/lists/*
```

**Workaround aplicado:** `dpkg --configure -a` en cada clone antes del install.

---

## F-3 `[TEMPLATE]/[DOC]` — el clean box no trae sshd corriendo ni usuario de bootstrap

**Síntoma:** en los spokes (worker/ingress/egress) el template arranca con **`ssh` inactivo** y
**sin usuario `administrator`** (solo `root` + `fluxbee`). `add_hive` hace bootstrap por SSH
(`ssh_user` requerido, default `administrator`; `ssh_password` key-first) → sin sshd + sin usuario
no hay forma de que el MB entre.

**Causa raíz:** el modelo de deploy es "spoke = caja Linux limpia con SOLO SSH, bootstrapeada por
el MB" (correcto para prod), pero (a) el template del lab no habilita sshd ni crea un usuario de
bootstrap, y (b) los prerequisitos del clean box no están documentados como contrato.

**Qué cambiar:**
- `[TEMPLATE]` (lab): habilitar `ssh` + crear el usuario de bootstrap (`administrator`, sudo,
  password conocido o la pubkey del MB) para que el clone sirva de spoke sin tocar nada.
- `[DOC]` (prod): documentar el contrato del clean box en 07-operaciones: **sshd activo, usuario
  sudo, y PasswordAuthentication on (si se usa `ssh_password`) o la pubkey del MB en
  `authorized_keys` (key-first)**. Hoy no está explícito.

**Workaround aplicado:** en 241/242/243 → habilitar sshd + crear `administrator:magicAI` (sudo
NOPASSWD) + PasswordAuthentication yes.

**Matiz (SSH password auth):** Ubuntu cloud-image trae
`/etc/ssh/sshd_config.d/50-cloud-init.conf` con `PasswordAuthentication no`. sshd es *first-match*,
así que un drop-in `99-*` NO lo pisa (50 < 99 gana). Hay que usar un drop-in de número **menor**
(ej. `00-fbi.conf`) o editar el del cloud-init. Impacto directo en `add_hive` con `ssh_password`:
si el deploy usa password, esto lo bloquea silenciosamente aunque el operador "haya habilitado"
password auth en un 99-. Documentar; idealmente el bootstrap debería preferir **key-first**
(pubkey del MB) y no depender de password.

---

## F-4 `[CODE]/[DOC]` — `externalize` / probe de edge no accesibles en un backend instalado

**Síntoma:** para cerrar el request-flow del edge (`curl /e/<ich>`) hay que **externalizar** un nodo
IO. Pero: (a) `externalize`/`list_externalized` NO están en la REST API del admin (`POST /admin
{action}` → `not_found`; solo hay rutas REST para `/hives`, `/hives/*/nodes`, `/hives/*/vault/*`);
(b) viven en el **dispatcher de acciones internas** (socket `admin.sock`), que el e2e invoca con un
binario **dev** `target/debug/admin_internal_command_diag` (no está en el `.deb`); (c) `sy-admin
--help` en el MB **cuelga** (no es un CLI de acciones). O sea: en una caja instalada no hay forma
"de fábrica" de externalizar/probar el edge sin herramientas de dev o `deploy-io-api.sh` desde un
checkout.

**Qué cambiar `[CODE]/[DOC]`:** exponer las acciones de canal por un camino operativo — sea REST
(`POST /channels/externalize`), sea un subcomando real de `sy-admin` (que hoy cuelga), sea empaquetar
un mini-cliente de socket en el `.deb`. Documentar el flujo de "publicar un nodo al edge" en
07-operaciones. Hoy el operador no tiene una vía obvia.

**Estado del edge (sí verificado):** `SY.edge@ingress1` activo, `:8443` LISTEN, responde `404` a
`/` cross-hive desde el MB (edge vivo, sin canal publicado). Falta solo el paso de publicar un ICH,
bloqueado por lo de arriba. **Request-flow completo = próximo test** (vía `deploy-io-api.sh
--edge-base http://192.168.4.212:8443` desde un checkout, o el camino operativo de F-4).

---

## F-5 `[CODE]` (menor) — `role` no se registra para el worker (default) en `/hives`

**Síntoma:** en `GET /hives`, `ingress1 role=ingress` y `egress1 role=egress`, pero `worker1
role=None`. El rol default (worker) no queda estampado/echoado en el inventario de hives.

**Qué cambiar `[CODE]`:** estampar `role="worker"` explícitamente al provisionar un spoke default,
para que el inventario sea consistente (hoy hay que inferir "worker" por ausencia de rol).

---

## Resumen — deploy integrado from-scratch en PC-004-157

**Topología levantada de cero** (apt install desde repo interno → firstboot → add_hive):

| Hive | Rol | Estado | Notas |
|------|-----|--------|-------|
| motherbee (240 @.210) | motherbee | alive, 18/18 nodos active | core SY.* + orchestrator + rt.gateway (:9000) + edge + IO/AI |
| worker1 (241 @.211) | worker | connected, api_healthy, dist-sync ok | bootstrap SSH + vendor-push OK |
| ingress1 (242 @.212) | ingress | connected, edge :8443 LISTEN | **dual-NIC** eth0 .212 + ens19 10.10.10.12 ✓ |
| egress1 (243 @.213) | egress | connected, NAT aplicado | **NAT** nft masquerade 10.10.10.0/24→eth0, ip_forward=1 ✓ |

**Funciona:** el deploy completo de la infra desde cero, los 4 roles, mesh mTLS (:9000, required),
orchestrator, rt.gateway, edge (listen+reachable), ingress dual-homed, egress NAT.

**Lo que aprendimos que hay que cambiar:** F-1 (template machine-id → colisión IP), F-2 (template
dpkg interrumpido), F-3 (clean box sin sshd/usuario de bootstrap + cloud-init pisa PasswordAuth),
F-4 (no hay vía operativa para externalizar/probar el edge en un backend instalado), F-5 (role del
worker no se estampa). F-1/F-2/F-3 son **arreglos de template/prep** → los scripts van al user.
