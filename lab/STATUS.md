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

**Resueltos (15):** `F1` `F2` `F9` `F10` `F14` `F20` `F7` `F12` `F13` `F3` `F11`
`F8` `F18` `F4` `F15` — código en `src/`. F1/F2/F9 validados en el lab; F10/F11 con
revisión adversarial (falta egress en VM); F14/F20 unit tests; **F7/F12/F13**,
**F3**, **F8**, **F18** unit tests + revisión adversarial (GO); **F11** (NO-GO →
corregido → re-verificado). **F4 + F15** = **autoridad de origen SYSTEM movida al
ROUTER** (gate en `serialize_for_local_delivery` sobre el `src_l2_name`
autoritativo; `SY.orchestrator@*` cross-hive + `SY.admin/wf-rules/diag@same`;
reglas `SY.` hardcodeadas + rutas **frozen** visibles en SHM; gate del orchestrator
eliminado). Validado en lab (cross-hive + same-hive + router-only, 0 drops) +
revisión adversarial (GO). **OPA-dual** = próximo gran feature para configurabilidad.

**Pendientes (9):**

- **Media:** `F5` `F6` `F16` `F17` (egress).
- **Baja:** `F19` `F21` `F22` `F23` `F25`.

> ✅ **F7/F12/F13 cerrados** (eran el #1 del audit). Un solo allowlist de iface
> (`^[A-Za-z0-9._-]{1,15}$`, IFNAMSIZ) en el único constructor de `EgressNatConfig`
> los cierra a los tres: el charset no comparte metacarácter con shell/nft/YAML.
> Además `write_remote_file` ahora manda el contenido por stdin a `sudo -n tee`,
> sin shell. **Siguiente prioridad: `F8`** (gate de origen en el canal ADMIN).

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

| Componente | Por qué |
| --- | --- |
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
- [x] **Lote de seguridad** del audit: ~~F7/F12/F13~~ ~~F3~~ ~~F11~~ (falta VM) ~~F8~~ ~~F18~~ ~~F4/F15~~ ✅ — todo el lote de seguridad **alta+origin-auth cerrado**. Quedan solo F5/F6/F16/F17 (egress, necesitan VM).
- [ ] **OPA-dual** (nuevo gran feature acordado): capa system no-editable + capa user con merge, en el router. Absorbe las reglas `SY.` hardcodeadas; herramienta para seguridad **y** bugs/diseño. Anotado para arrancar como proyecto aparte.
- [ ] **Lote egress**: F5/F6/F16/F17 + validar F10/F11 en **VM** (Fase 2 del lab).
- [x] **Distribución**: workflow `.github/workflows/lab-image.yml` pushea a GHCR las dos flavors — `slim` (multi-stage, ~1.9 GB, `:latest`, boot validado) y `fat` (~9 GB). Repo público → GHCR + Actions **gratis e ilimitado**. **Falta disparar**: `git tag lab-v0.1 && git push --tags` (o Run workflow).
- [ ] Distribución (mejoras): slim aún duplica binarios (`/usr/bin` + `dist/core/bin`) y syncthing — se puede bajar más. Multi-arch (arm64) si hay devs en Apple Silicon.

## Para investigar (anotado 2026-06-26)

- **Vista de ilks por hive en SY.architect (posible bug).** Al pedir "ilks por
  hive", `worker1` devuelve **19** (sus 7 propios + los 12 de `motherbee`),
  mientras `motherbee` devuelve 12. Cada ilk viene **etiquetado con su hive
  dueño** (p.ej. `SY.storage@motherbee`), así que el dato parece la **vista
  replicada de la malla**, no corrupción. Hipótesis a discernir:
  1. Replicación de SY.identity **por diseño** (cada hive sincroniza ilks vía
     `identity.sync` puerto 9100) + archi/admin lo presenta como "ilks de
     worker1" → bug de **presentación / scoping de la query** en archi o en el
     endpoint admin.
  2. El listado de ilks por hive **debería filtrar local-only** y no lo hace →
     bug de **scoping** en admin / SY.identity.
  3. El orchestrator no levantó bien algún nodo.
  - Dónde mirar: endpoint admin de list-ilks (scoping por hive), replicación de
    SY.identity (SHM/sync 9100), y cómo archi arma la consulta. Repro disponible
    en el lab (motherbee + worker1, 21 nodos).
- [ ] **AI**: cargar OpenAI key reproducible; scope/ejecución de la **extensión Anthropic**.
