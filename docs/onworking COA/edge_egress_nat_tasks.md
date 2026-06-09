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

- [x] **T-ROLE-1** `is_egress_role()` junto a `is_mother_role`/`is_worker_role`. ✓
- [x] **T-ROLE-2** Aceptar `egress` en el gate de arranque. Mensaje `role=motherbee|worker|egress`; chequeo `hive_id == PRIMARY_HIVE_ID` extendido a `!is_motherbee` (egress tampoco puede ser primary). ✓
- [x] **T-ROLE-3** `EgressSection` struct + `egress: Option<EgressSection>` en `HiveFile`. Campos completos + `default_ipv6_policy()`. ✓ *(campos se consumen en §5; warning dead-code esperado hasta Fase B)*
- [x] **T-ROLE-4** `enum HiveRole { Motherbee, Worker, Egress }` + `from_role()` + `as_str()`. Campo `role: HiveRole` agregado a `OrchestratorState` (resuelto una vez en startup). (Cierra F6.) ✓
- [x] **T-ROLE-5** `system_nodes_for_role(&HiveFile, HiveRole)`. Call-sites actualizados: startup, `restart_local_core_services` (resuelve rol del hive), `worker_core_component_names` (→ Worker, push de provisioning), `add_hive_flow` worker yaml (→ Worker). Además se hicieron role-aware `core_component_names_for_role`/`core_bin_paths_for_role`/`compute_local_core_update_sets` (self-management usa `state.role`) y se factoró `core_component_names_from_section`. (Cierra F5.) ✓
- [x] **T-ROLE-6** `SystemNodesSection.egress: Option<RoleSystemNodes>`. ✓
- [x] **T-ROLE-7** `validate_system_nodes(section, HiveRole)`: invariante `SY.config.routes` primero para todos los roles; vault-checks sólo motherbee; "no `sy-vault`" extendido a `role != Motherbee`; mensajes usan `role.as_str()`. ✓
- [x] **T-ROLE-8** `render_system_nodes_yaml(role, section)` parametrizada (reemplaza `render_worker_system_nodes_yaml`). ✓
- [x] **T-ROLE-9** Provisioning remoto del hive egress: función **dedicada** `add_egress_hive_flow` (no ramifica la worker, que tiene 133 touchpoints worker-específicos). Reusa los helpers de bootstrap SSH / sudoers / `sync_core_to_worker` (ahora con param `role`) / `write_remote_file` / systemd / `wait_for_wan` / `write_hive_info` / `apply_add_hive_ssh_controls_after_finalize`. Emite yaml `role: egress` + bloque egress (con `edge_ip` derivado) + `system_nodes.egress` (template de motherbee). Params host-specific desde el payload de `add_hive` (`resolve_add_hive_role` + `resolve_add_hive_egress_section`). ✓

> **Estado:** Fase A + provisioning egress completos y compilando; `cargo test --bin sy_orchestrator` → **99 ok** (egress: derivación edge_ip, validación config, ruleset determinista, egress rechaza vault / acepta routes-only, parseo de role, egress payload y gateway worker). Un hive `role: egress` se reconoce, arranca con su perfil, auto-gestiona componentes; y `add_hive role=egress` lo provisiona remotamente.

---

## 5) Orchestrator — network reconciliation (core work)

Greenfield (F1). Corre **localmente** en el host egress, gated por `is_egress && egress.enabled`, siguiendo el patrón idempotente de bloque marcado de los helpers `_local` existentes pero contra **nftables**.

- [x] **T-NET-1** `command_exists("nft")` en `reconcile_egress_nat`; fail-loud si falta (sin fallback). ✓
- [x] **T-NET-2** Writer `egress_sysctl_content()` → `/etc/sysctl.d/99-fluxbee-egress.conf` + `apply_sysctl_system()`. IPv4 forward on; IPv6 fully off. ✓
- [x] **T-NET-3** `egress_nft_ruleset()` → tabla dedicada `inet fluxbee_egress` con chains `input` (D1 hook, policy accept), `forward` (drop; ct est/rel; LAN→WAN; `meta nfproto ipv6 drop`) y `postrouting` (masquerade). Sustitución desde `EgressNatConfig`. El reconcile elimina la tabla Fluxbee existente si está cargada y aplica la definición limpia. ✓
- [x] **T-NET-4** **(D1)** `ensure_core_firewall_local` hace early-return en `role=egress` (no usa ufw). Inbound queda como base chain `input` en la tabla fluxbee (policy accept en v1). *Nota: el filtrado inbound real (drop + allows explícitos) se difiere a §11 para no arriesgar lockout en la box de testing; D1 se cumple en cuanto a "nft es el único backend, ufw no se invoca".* ✓
- [x] **T-NET-5** conntrack: `apply_conntrack_live()` (sysctl -w tolerante) + `/etc/sysctl.d/99-fluxbee-conntrack.conf` + `/etc/modprobe.d/fluxbee-conntrack.conf` (hashsize, next-boot). ✓
- [x] **T-NET-6** `nft -f` + `nft_table_loaded()` verifica que la tabla quedó cargada; error si no. ✓
- [x] **T-NET-7** `worker_ipv6_sysctl_content()` (disable_ipv6 + accept_ra=0, sin forwarding) aplicado en `reconcile_worker_egress`. Camino `EGRESS_IPV6_UNMANAGED` si `ipv6 != "blocked"`. ✓
- [x] **T-NET-8** Idempotencia: `write_file_if_changed` (compare-and-write, archivos dedicados) + replace explícito de la tabla nft dedicada. Test `egress_nft_ruleset_is_deterministic_*`. ✓

---

## 6) Config / derivación

- [x] **T-CFG-1** `resolve_egress_nat_config`: `lan_cidr`/`wan_iface`/`lan_iface` requeridos; `ipv6` sólo `"blocked"` (fail-loud). Test `resolve_egress_nat_config_requires_fields_and_blocks_ipv6`. ✓
- [x] **T-CFG-2** Sección `egress` en `hive.yaml` de motherbee (`gateway_ip` + `edge_hive`): deserializa a `state.egress` y la inyección en `add_hive_flow` ya la consume (T-WRK-1). ✓
- [x] **T-CFG-3** `first_usable_ipv4()` con bit-math std, sin crate; deriva gateway sólo para CIDRs con host usable (`/31` y `/32` requieren `edge_ip` explícito). Test `first_usable_ipv4_handles_any_mask`. ✓

---

## 7) Worker route injection (D2)

- [x] **T-WRK-1** `add_hive_flow` para `role=worker`: inyecta `egress:\n  gateway_ip: ...\n  ipv6: "blocked"` en el yaml generado, tomado de `state.egress.gateway_ip` (la declaración de motherbee). ✓ **→ Mode B end-to-end completo.**
- [x] **T-WRK-2** `reconcile_worker_egress` (vía `reconcile_egress` en `bootstrap_local`): si `egress.gateway_ip` presente, `ip route replace default via <gw>` (idempotente, reaplicada cada boot) + bloqueo IPv6. ✓
- [~] **T-WRK-3** Verificación local hecha (`ping_internet` + tracing). Falta emitir el **JSON payload** del worker hacia motherbee (va con provisioning, §8).
- [~] **T-WRK-4** Mode B: el código no distingue origen del gateway (transparente) → funciona en cuanto T-WRK-1 inyecte `gateway_ip`. Pendiente sólo de T-WRK-1.

---

## 8) Verification (no silent success)

- [x] **T-VER-1** Respuesta JSON de `add_hive role=egress` incluye `egress_role`, `egress_lan_cidr`, `egress_edge_ip`, `egress_wan_iface`, `egress_lan_iface`, `wan_connected`, `orchestrator_connected`. Los campos de NAT en sí (`nat_applied`/`ipv6_blocked`/`internet_reachable`) los computa y loguea el orchestrator **del host egress** al bootear (no viajan a motherbee en v1; quedan en su journal). El payload lo dice explícitamente en `note`. ✓
- [x] **T-VER-2** **(decidido)** `ping_internet()` = ping ICMP a `fluxbee.ai` (`-c 1 -W 2`). IP-echo/IP público diferido. ✓
- [~] **T-VER-3** Worker: `route_applied` + `internet_reachable` capturados y logueados. Falta el JSON hacia motherbee (provisioning).
- [x] **T-VER-4** Si `ipv6 != "blocked"` → `EGRESS_IPV6_UNMANAGED` warn; el ping confirma IPv4 por separado. ✓

> **Estado:** reconciliación local y provisioning remoto están implementados y testeados por unit tests (`cargo test --bin sy_orchestrator` → 99 ok). Siguen pendientes las pruebas de integración §9 sobre máquina real (host con `nft`/2 NICs/router físico) y el JSON worker hacia motherbee de T-VER-3.

---

## 9) Validación / tests

- [x] **T-TST-1** `role=egress` pasa `validate_system_nodes` con perfil sólo-`SY.config.routes`.
- [x] **T-TST-2** `role=egress` rechazado si lista `sy-vault`.
- [x] **T-TST-3** Ruleset nft determinista y replace de tabla dedicada sin preámbulo duplicador (T-NET-8). *La re-aplicación contra nft real queda cubierta por prueba de host real.*
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

## 10.5) Fixes post code-review (high effort)

- [x] **CR-1** `reconcile_egress` fatal en bootstrap para `role: egress` (NAT es su razón de ser; falla ⇒ orchestrator no "active" ⇒ `add_hive` reporta `SERVICE_FAILED`, no "ok" silencioso). Worker = warn no-fatal.
- [x] **CR-2** `add_egress_hive_flow` early-return (`WAN_TIMEOUT`/`ORCHESTRATOR_TIMEOUT`) antes de endurecer SSH, simétrico con worker.
- [x] **CR-5** `watchdog_egress_reconcile` (tick 5s): egress re-aplica NAT sólo si la tabla nft desapareció; worker re-aplica `ip route replace` idempotente. Cierra el gap §6.5 "on startup **and on reconcile**".
- [x] **CR-6** `resolve_add_hive_role` rechaza `role` no-string en vez de defaultear a worker.
- [x] **CR-3** Forward `policy drop` en tabla dedicada — **se deja como está**. El nodo egress es infra dedicada (no hace otro forwarding), así que el invariante "host dedicado" lo cubre; no se agrega chequeo de coexistencia.
- [x] **CR-4** `check_internet_reachable`: ping ICMP **+ fallback GET HTTPS** a `https://fluxbee.ai`. Elimina el falso negativo (redes que filtran ICMP pero permiten 443) y doble propósito: si el website responde, la infra/fluxbee cloud está viva.

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
