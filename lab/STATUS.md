# Fluxbee — estado del trabajo (lab + auditoría)

_Actualizado: 2026-06-25 · Branch: `daily_onworking_coa`_

Snapshot consolidado para retomar sin perder hilos. Tres frentes entrelazados:
(1) fixes de la auditoría `sy.orchestrator`, (2) cambio de diseño de las
credenciales de bootstrap, (3) el lab containerizado que valida lo anterior.

---

## 1. Auditoría sy.orchestrator

Origen: [`docs/audits/2026-06-23-sy-orchestrator-audit.md`](../docs/audits/2026-06-23-sy-orchestrator-audit.md)
(24 findings; ver ahí el **"Estado de resolución"** con el detalle por finding).
Prioridad acordada: **operativos primero, seguridad después** (red interna con
puertas definidas; SSH solo para bootstrap inicial).

**Resueltos (6):** `F1` `F2` `F9` `F10` `F14` `F20` — código en `src/` **sin
commitear aún**. F1/F2/F9 validados empíricamente en el lab; F10 con revisión
adversarial (falta validación en egress); F14/F20 con unit tests.

**Pendientes (18):**

- **Alta:** `F7` (RCE root vía iface — **el #1 del audit, sigue abierto**),
  `F8` (gate de origen ADMIN), `F3` (traversal hive_id), `F4` (allowlist),
  `F11` (egress NAT teardown).
- **Media:** `F12`/`F13` (inyección nft/YAML — mismo fix que F7), `F5` `F6`
  `F16` `F17` (egress), `F15` (cross-hive), `F18` (TOCTOU lock).
- **Baja:** `F19` `F21` `F22` `F23` `F25`.

> ⚠️ **F7 es el de mayor prioridad pendiente.** El lote de creds tocó el
> threading de `write_remote_file` pero **no** el shell-injection ni la
> validación de nombre de interfaz.

---

## 2. Credenciales de bootstrap (cambio de diseño del operador)

`ssh_user` (requerido) / `ssh_password` (opcional) **en el payload de
`add_hive`**; probe **key-first**; sin `administrator`/`magicAI` hardcodeados;
secreto **redactado** en `SY.architect`; schema de admin actualizado.

- **Validado** en el lab: worker real bootstrapeado con las creds del payload;
  `ssh_user` persistido en info.yaml, `ssh_password` nunca.
- **Pendiente:** review adversarial del código de este lote + actualizar scripts
  E2E (`admin_add_hive_matrix.sh` etc. ahora deben mandar `ssh_user`).

**Modelo de deploy a mantener:** SSH solo para instalar el orchestrator en una
caja Linux vacía, con user/pass que provee quien llama `add_hive` (humano o
archi); después el orchestrator entra por key y cierra el password en el nodo.
Todo lo demás (mesh, updates) por socket/syncthing — sin SSH operativo.

---

## 3. Lab containerizado (committed)

`lab/` — systemd-in-docker que corre **fluxbee real** sin tocar su código.

- **Fase 0** (commit `861a182`): motherbee completo, zero-touch (~2 min), 16
  servicios.
- **Fase 1** (commit `b486ac5`): worker vacío + `add_hive` over SSH → **malla de
  2 hives, 21 nodos**.
- Uso: `docker compose -f lab/docker-compose.yml up -d` · ver [`lab/README.md`](README.md).

**Qué funciona completo en Docker:** todo el plano de control —
orquestación, identity, vault (Model D'), storage, cognition, dist-sync,
`add_hive`/`remove_hive`, la mesh, nodos AI (con key).

**Limitado en Docker (necesita VM):**

| | Por qué |
|---|---|
| Rol **egress/NAT** | `ip_forward`, `nft MASQUERADE`, conntrack — kernel netfilter (F10/F11/F17). |
| Firewall **ufw** | queda `inactive` en el container — el hardening de firewall no se aplica. |
| Boot/reboot | restart de container ≠ reboot de VM (afecta cosas de arranque de egress). |

**Validación empírica lograda (malla real):** F1 (online+offline), F2, F9,
y el flujo de creds end-to-end.

---

## Secretos en el vault del lab

- **postgres** — cargado automático por `lab-install.sh` (`resource_type=postgres`).
- **OpenAI** — NO cargado. Para nodos AI: `vault_put` con `resource_type=openai`,
  value `{"api_key":"sk-..."}`. La key es secreto del operador → no se commitea
  (plan: leerla de `lab/secrets/openai.key` gitignored y auto-cargar al boot).
- **Anthropic** — el enum `ResourceType` **ya lo soporta**, pero el consumo de AI
  (`sy_cognition`, `ai_node_runner`, architect) resuelve `Openai` y pega a
  `api.openai.com`. Extenderlo a Anthropic = **cambio de código** (lote aparte).

---

## Pendientes vivos (checklist)

- [ ] **Commit** de los cambios `src/` del audit (F1/F2/F9/F10/F14/F20 + creds) — separado del lab.
- [ ] **Review adversarial** del código del lote creds.
- [ ] Actualizar **scripts E2E** (`ssh_user` requerido) + docs de contrato (`sy_orchestrator_v2_tasks.md` items F1/F2).
- [ ] **Lote de seguridad** del audit: F7/F12/F13 (iface), F8, F3, F4/F15, F11.
- [ ] **Lote egress**: F5/F6/F16/F17 + validar F10/F11 en **VM** (Fase 2 del lab).
- [ ] **Distribución**: imagen **slim multi-stage** + push a registry (GHCR / Docker Hub) para compartir sin recompilar; multi-arch si hay devs en Apple Silicon.
- [ ] **AI**: cargar OpenAI key reproducible; scope/ejecución de la **extensión Anthropic**.
