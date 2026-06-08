# Edge Egress NAT — Implementation Tasks

**Status:** planning / ready for implementation
**Date:** 2026-06-08
**Type:** new capability — `role: egress` + OS-level NAT reconciliation
**Primary spec:** `docs/edge-egress-nat-spec.md` (v1.0)
**Target module:** `src/bin/sy_orchestrator.rs` (single binary, ~19.3k líneas)
**Related:** `edge-control-protocol.md` (ingress, fase posterior), `01-arquitectura.md`, `05-conectividad.md`, `07-operaciones.md`

---

## 1) Goal

Agregar un tercer rol de hive (`egress`) y la reconciliación de red OS-level (nftables/sysctl/conntrack) que le permite a un host LAN-only dar salida HTTPS a internet a los workers internos, sin abrir ningún inbound de internet hacia los workers. Soporta dos modos:

- **Mode A** — hive Fluxbee `role: egress` que Fluxbee gestiona end-to-end (NAT propio).
- **Mode B** — router/firewall físico preexistente; Fluxbee sólo inyecta la ruta por defecto a los workers. `role: egress` no se usa.

El spec está escrito con precisión contra el código actual: todos los números de línea citados calzan exacto al momento de redactar este doc (`HiveFile`@132, `is_mother_role`@16106, gate de arranque@503, `system_nodes_for_role`@2887, `validate_system_nodes`@2910, `render_worker_system_nodes_yaml`@3030, helpers de firewall@3331/3391/3473, `add_hive_flow`@14061, yaml worker@14580).

---

## 2) Decisiones congeladas

Estas dos decisiones divergen del spec v1.0 y son la fuente de verdad para este backlog (priman sobre el spec donde haya conflicto):

- **D1 — Backend de firewall en hosts egress: nftables único, inbound migrado.** En un host `role: egress` el orchestrator deja de usar `ufw` y gestiona **tanto inbound (WAN/identity) como egress (forward/NAT)** por nftables. Un solo backend posee las reglas en ese host. (Spec §8.5 ofrecía "mantener ufw + tabla separada" o "desactivar ufw"; elegimos el backend único nft para egress, evitando la contradicción de desactivar ufw mientras se siguen abriendo puertos por ufw). **No afecta** a motherbee/worker normales, que siguen con `open_firewall_rules_local`/ufw como hoy.

- **D2 — Ruta del worker: `gateway_ip` en el `hive.yaml` del worker + reconcile local.** En vez del push SSH one-shot del §7, motherbee escribe `egress.gateway_ip` dentro del `hive.yaml` generado para el worker, y el **orchestrator del worker** aplica + reconcilia la ruta por defecto y el bloqueo IPv6 **localmente en cada arranque/reconcile**. Coherente con el patrón local del codebase (el gate de arranque y los helpers `_local` ya son locales) y con el principio event-driven / reconcile (memoria `feedback_event_driven_over_polling`). Persistente por diseño: sobrevive reboots porque se reaplica en cada boot. Esto **resuelve además** el problema de persistencia del §7 (un `ip route add` runtime no sobrevive netplan/NM/reboot).

### Implicación de D2 en el spec §6.6 / §7

- El worker yaml **sí** lleva la ruta (`egress.gateway_ip`), contradiciendo el §7 ("not encoded in the worker hive.yaml"). Actualizar §7 del spec.
- `add_hive_flow` para un worker NO ejecuta `ip route` por SSH; sólo inyecta el campo en el yaml. La aplicación es local en el worker.
- El verification payload del worker (§9) lo emite el orchestrator del worker tras aplicar local, no motherbee tras el push.

---

## 3) Fricciones detectadas (notas técnicas)

| # | Fricción | Impacto | Resolución en este backlog |
|---|----------|---------|----------------------------|
| F1 | **nftables es greenfield**: `grep nft` en `src/` = 0. Los helpers actuales sólo conocen `ufw`/`firewall-cmd`. | Alto: el grueso del trabajo (§6.5/#9) no se apoya en nada existente. | Tareas en §5 (network reconciliation). Nuevo módulo de escritura de bloque nft marcado + idempotencia + verificación de ruleset cargado. |
| F2 | `disable_ipv6=1` global desactiva IPv6 también en loopback `::1`. | ~~Medio~~ → **resuelto, seguro**. | **Verificado en código**: NATS embebido bindea `127.0.0.1` (IPv4, vía `endpoint_to_addr`); RT.gateway WAN es `TcpListener::bind` sobre dirección IPv4 del yaml; router node/IRP son Unix domain sockets (IP-agnósticos). Nadie bindea `::1`/`[::]` (único match de `::` en `sy_orchestrator.rs:15979` es un comparador de host-string, no un bind). Desactivar IPv6 global en workers es seguro. |
| F3 | conntrack `hashsize` vía `modprobe` sólo aplica al **cargar** el módulo → efectivamente reboot-only. `nf_conntrack_max` por sysctl sí es live. | Bajo: expectativa incorrecta de "tuned" inmediato. | T-NET-5: aplicar max por sysctl live; escribir `modprobe.d` para próximo boot; reportar `egress_conntrack_tuned` distinguiendo "live" de "pending-reboot". Documentar en `07-operaciones.md`. |
| F4 | ~~`edge_ip` derivación sin crate de CIDR~~ → **resuelto, no es problema**. | Trivial. | **Sin crate**: `(network & mask) + 1` con bit-math `Ipv4Addr ↔ u32` de `std::net` vale para cualquier máscara (/16../29), ~8 líneas. No cambia la config (`edge_ip` ya es overridable; la derivación es sólo el default). Cierra Open Question #2 del spec. |
| F5 | **Lista de call-sites del spec §6.2 incompleta**: cita 542/2166/2169, pero `system_nodes_for_role` también se llama en **13069 y 14444** (ambas pasan `false` literal hoy). | Bajo, pero rompe compilación si se omite. | T-ROLE-5 incluye explícitamente 13069 y 14444. |
| F6 | El spec usa `role.as_str()` (§6.2/6.4) sin definir el `impl` del `HiveRole` enum. | Trivial. | T-ROLE-4 define `enum HiveRole` + `as_str()`. |
| F7 | Verificación de salida a internet. | Bajo. | **Decidido**: v1 sólo confirma "hay internet" con un **ping a `fluxbee.ai`** (ICMP). El descubrimiento del IP público (`egress_public_ip` vía IP-echo) se difiere al sistema completo (verificación más compleja). |

---

## 4) Orchestrator — role plumbing (mecánico)

Cambios de bajo riesgo, cross-cutting en el binario único.

- [ ] **T-ROLE-1** `is_egress_role()` junto a `is_mother_role`/`is_worker_role` (cerca de 16106).
- [ ] **T-ROLE-2** Aceptar `egress` en el gate de arranque (503–512). Mensaje: `role=motherbee|worker|egress`. Mantener el chequeo de `hive_id == PRIMARY_HIVE_ID` sólo para motherbee; egress es un hive normal (no primary).
- [ ] **T-ROLE-3** `EgressSection` struct + campo `egress: Option<EgressSection>` en `HiveFile` (132). Campos: `enabled`, `lan_cidr`, `edge_ip`, `wan_iface`, `lan_iface`, `ipv6` (default `"blocked"`), `gateway_ip`, `edge_hive`.
- [ ] **T-ROLE-4** `enum HiveRole { Motherbee, Worker, Egress }` + `impl HiveRole { fn as_str(&self) -> &'static str }`. (Cierra F6.)
- [ ] **T-ROLE-5** Refactor `system_nodes_for_role(&HiveFile, HiveRole)` (2887) desde `is_motherbee: bool`. Actualizar **todos** los call-sites: 542, 2169, **13069**, **14444** (estos dos pasan `false` literal hoy → `HiveRole::Worker`). (Cierra F5.)
- [ ] **T-ROLE-6** `SystemNodesSection.egress: Option<RoleSystemNodes>` (146).
- [ ] **T-ROLE-7** `validate_system_nodes` role-aware (2910): mantener invariante "`SY.config.routes` debe ir primero" para todos los roles; saltar chequeos de vault para no-motherbee; extender el chequeo "no debe correr `sy-vault`" de `!is_motherbee` a `role != Motherbee` (2989) — egress tampoco corre vault.
- [ ] **T-ROLE-8** Parametrizar el render de system_nodes yaml para emitir la clave de rol correcta. El spec sugiere `render_system_nodes_yaml(role, section)` unificada; renombrar/generalizar el actual `render_worker_system_nodes_yaml` (3030) en lugar de duplicar por rol.
- [ ] **T-ROLE-9** Generación del `hive.yaml` egress en `add_hive_flow` (14580): emitir `role: egress` + bloque `egress` (`lan_cidr`, `edge_ip`, `wan_iface`, `lan_iface`, `ipv6`) + `system_nodes.egress`. Distinto del `format!` de worker hardcodeado actual.

---

## 5) Orchestrator — network reconciliation (core work)

Greenfield (F1). Corre **localmente** en el host egress, gated por `is_egress && egress.enabled`, siguiendo el patrón idempotente de bloque marcado de los helpers `_local` existentes pero contra **nftables**.

- [ ] **T-NET-1** Detección de backend: `command_exists("nft")`. Si falta nft en un host egress → fail-loud (no fallback silencioso a ufw/iptables).
- [ ] **T-NET-2** Writer sysctl `/etc/sysctl.d/99-fluxbee-egress.conf` (bloque `# BEGIN/END FLUXBEE EGRESS`) + `sysctl --system`. IPv4 forward on; IPv6 fully off (forwarding off, disable_ipv6, accept_ra=0).
- [ ] **T-NET-3** Writer nftables: tabla dedicada `table inet fluxbee_egress` (auto-contenida, recomendada en Open Question #3) con chains `forward` (policy drop; ct established/related accept; LAN→WAN accept; `meta nfproto ipv6 drop`) y `postrouting` (masquerade LAN out wan_iface). Sustituir `lan_cidr`/`lan_iface`/`wan_iface` desde la `EgressSection`. Bloque marcado idempotente.
- [ ] **T-NET-4** **(D1)** En hosts egress, migrar las reglas **inbound** (WAN/identity ports que hoy van por `open_firewall_rules_local`/ufw) a nftables dentro de la misma tabla/política. Sólo para `role: egress`; motherbee/worker siguen igual. Definir cómo se ramifica el código de firewall por rol sin tocar el camino ufw existente.
- [ ] **T-NET-5** conntrack: `nf_conntrack_max` por sysctl (live) + `modprobe.d` `hashsize=` (next-boot). Reportar estado live vs pending-reboot (F3).
- [ ] **T-NET-6** Aplicar nft (`nft -f`) + **verificar** que el ruleset quedó cargado (`nft list table inet fluxbee_egress`), no asumir éxito.
- [ ] **T-NET-7** Bloqueo IPv6 en workers (sysctl disable_ipv6 + accept_ra=0, **sin** las líneas de forwarding/NAT). Aplicado localmente por el orchestrator del worker (D2). Seguro: ningún servicio bindea `::1`/`[::]` (F2 verificado). Camino `EGRESS_IPV6_UNMANAGED` si no se puede aplicar.
- [ ] **T-NET-8** Idempotencia: re-aplicar nft/sysctl no produce drift (bloques marcados reemplazados en sitio, no apilados).

---

## 6) Config / derivación

- [ ] **T-CFG-1** Validación de `EgressSection` cuando `enabled`: `lan_cidr`, `wan_iface`, `lan_iface` requeridos en hive egress; `ipv6` sólo acepta `"blocked"` en v1 (rechazar otros valores fail-loud).
- [ ] **T-CFG-2** Sección `egress` en `hive.yaml` de motherbee: `gateway_ip` (requerido para habilitar inyección) + `edge_hive` (informativo). Si `gateway_ip` ausente → `add_hive` no inyecta ruta.
- [ ] **T-CFG-3** Derivación `edge_ip` = primera IP usable de `lan_cidr` si ausente: `(network & mask) + 1` con bit-math `Ipv4Addr ↔ u32` de `std::net`, válido para cualquier máscara, **sin crate** (F4 cerrado).

---

## 7) Worker route injection (D2)

- [ ] **T-WRK-1** `add_hive_flow` para `role=worker`: cuando motherbee tiene `egress.gateway_ip`, inyectar el campo `egress.gateway_ip` en el `hive.yaml` generado para el worker. **No** ejecutar `ip route` por SSH.
- [ ] **T-WRK-2** Orchestrator del worker: al arrancar/reconciliar, si `egress.gateway_ip` presente, aplicar localmente la ruta IPv4 por defecto al gateway (persistente/reaplicada cada boot) + bloqueo IPv6 (T-NET-7).
- [ ] **T-WRK-3** Verificar salida HTTPS a través del gateway y emitir verification payload del worker (§9) desde el propio worker.
- [ ] **T-WRK-4** Mode B: workers reciben `gateway_ip` apuntando al router físico, sin que exista ningún hive egress. Mismo camino que T-WRK-1/2 (el origen del gateway es transparente).

---

## 8) Verification (no silent success)

- [ ] **T-VER-1** Payload de hive egress: `egress_role`, `egress_nat_applied`, `egress_ipv4_forwarding`, `egress_ipv6_blocked`, `egress_conntrack_tuned` (live|pending-reboot), `egress_wan_iface`, `egress_lan_iface`, `egress_internet_reachable`.
- [ ] **T-VER-2** **(decidido)** v1: `egress_internet_reachable` = **ping ICMP a `fluxbee.ai`** desde el host. Sólo confirma "hay internet por el path", no descubre IP público. El `egress_public_ip` vía IP-echo HTTPS queda diferido al sistema completo (verificación más compleja).
- [ ] **T-VER-3** Payload de worker: `egress_configured`, `egress_gateway_ip`, `egress_ipv4_ready`, `egress_ipv6_blocked`, `egress_internet_reachable` (ping a `fluxbee.ai` a través del gateway).
- [ ] **T-VER-4** Si IPv4 funciona pero IPv6 no quedó bloqueado → warn o fail según modo de deployment. No éxito parcial silencioso.

---

## 9) Validación / tests

- [ ] **T-TST-1** `role=egress` pasa `validate_system_nodes` con perfil sólo-`SY.config.routes`.
- [ ] **T-TST-2** `role=egress` rechazado si lista `sy-vault`.
- [ ] **T-TST-3** Re-aplicar nft/sysctl idempotente: sin drift (T-NET-8).
- [ ] **T-TST-4** Worker alcanza internet por el gateway; IPv6 confirmado bloqueado.
- [ ] **T-TST-5** Mode B: workers reciben ruta desde `gateway_ip` sin hive egress presente.
- [ ] **T-TST-6** **(D1)** En host egress, inbound (WAN/identity) sigue alcanzable con backend nft (no ufw).
- [ ] **T-TST-7** **(D2)** Worker reaplica ruta + IPv6 block tras reboot/reconcile (persistencia por reconcile, no por config de red persistente externa).

---

## 10) Fuera de v1 (hardening, reservado)

Del §11 del spec — no implementar en esta tanda, sólo reservar hooks:
- FQDN/SNI egress allow-list (inserta en chain `forward` de T-NET-3, antes del accept LAN→WAN).
- Observabilidad/audit (nft `log` en forward chain; NetFlow/sFlow).
- Rate-limit por worker (nft `limit` por saddr).
- Resolver DNS designado (DoT upstream en egress hive).
- Egress HA / VRRP (egress hive es SPOF hoy).
- Aislamiento de proceso/host (AppArmor/SELinux).
- Reconcile de workers preexistentes (D2 lo mitiga parcialmente: los workers ya reconcilian local, pero los provisionados **antes** de declarar `gateway_ip` no tienen el campo en su yaml → requiere re-emisión del yaml, acción de operador).

La chain `forward` de T-NET-3 se estructura para que allow-list y logging inserten sin reestructurar.

---

## 11) Open questions a cerrar durante implementación

1. **Piso del perfil egress** (Open Question #1): confirmar que el router local bootea limpio con sólo `SY.config.routes` (+ `RT.gateway`/`SY.orchestrator` implícitos). Si necesita otro SY node para registrarse, agregarlo y actualizar spec §5.1.
2. ~~**`edge_ip` para máscaras ≠ /24** (Open Question #2 / F4)~~ → **cerrado**: bit-math std `(network & mask)+1`, cualquier máscara, sin crate.
3. ~~**Mecanismo de include nftables** (Open Question #3)~~ → **decidido**: tabla dedicada `inet fluxbee_egress`, auto-contenida. Confirmar no-conflicto con política nft preexistente en la box real.
4. ~~**Endpoint IP-echo** (Open Question #4)~~ → **cerrado**: v1 usa ping a `fluxbee.ai` (T-VER-2). IP-echo diferido.
5. **conntrack defaults** (Open Question #5): 262144 / hashsize 65536 son puntos de partida; ajustar a escala.

---

## 12) Actualizaciones de doc requeridas al cerrar

- `edge-egress-nat-spec.md` §7: la ruta del worker **sí** va en el yaml (D2), corregir el "not encoded in the worker hive.yaml".
- `edge-egress-nat-spec.md` §8.5: backend único nft en egress (D1), descartar la opción "desactivar ufw".
- `edge-egress-nat-spec.md` §9: verificación v1 = ping `fluxbee.ai` (`egress_internet_reachable`), no `egress_public_ip` vía IP-echo (diferido).
- `07-operaciones.md`: guía de escalado conntrack (monitorear `nf_conntrack_count` vs `_max`, subir al ~70% sostenido) + nota hashsize next-boot.
- Añadir este archivo al índice `docs/onworking COA/README.md`.
