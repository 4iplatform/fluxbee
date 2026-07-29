# METHOD — cómo se trabaja la infraestructura Fluxbee

> Documento de método. Define **cómo** se opera la infra (lab y prod), **qué puede hacer el agente**,
> y **cómo se registra cada cambio**.
>
> Documentos hermanos en este directorio:
> - `YYYY-MM-DD.md` — **la bitácora**: el viaje, cambio por cambio, con fecha/hora/evidencia.
> - `FINDINGS.md` — **los hallazgos acumulados** del despliegue integrado; de ahí sale el
>   **plan de cambios de código** al terminar. Durante el despliegue se **agregan**, no se arreglan.
> - `HANDBOOK.md` — **las recetas validadas** (portables, listas para el próximo prod). Una receta
>   se escribe **recién después** de haberla ejecutado con éxito acá.
>
> Complementa —no reemplaza— a [`lab/PROXMOX.md`](../PROXMOX.md) (operación diaria del lab) y
> `docs/packaging-and-build.md` (build/paquetes). Este doc es el que manda cuando hay prod de por medio.

---

## 1. Entornos

| | **LAB** | **PROD** |
|---|---|---|
| Proxmox | `192.168.4.165` (PC-004-165), `192.168.4.157` (PC-004-157) | **`192.168.8.207`** (nodo `pve`) |
| Uso | experimentar, romper, validar | servicio real |
| Build box | fb-build VM210 @165 · repo apt `192.168.4.200:8900` | *(no tiene: los binarios se traen del lab)* |
| Hives | 240–243 @157 | *(a construir)* |
| Red | 192.168.4.0/24 | 192.168.8.0/24, gw `192.168.8.1`, bridge `vmbr0` |
| Timezone del host | — | `America/New_York` (**el journal del host va en EDT, la bitácora en ART −03**) |

**Las dos redes no se ven entre sí.** El puente es la máquina del operador/agente, que alcanza ambas:
para llevar un `.deb` o un binario de lab→prod se baja del build box y se sube a prod. No se conectan las redes.

---

## 2. Capacidades del agente (verificado empíricamente, no asumido)

Token prod `ai-agent@pve!vscode` → ACL `/ = Administrator, propagate=1`, `privsep=0`.
**Es admin total del datacenter.** En concreto:

### Nivel 1 — Datacenter vía API REST: **SÍ, todo**
- **VMs**: crear, clonar, arrancar/parar/destruir, snapshots, rollback, migrar.
- **Templates**: crear (VM + `template=1`) y clonar desde ellos.
- **Storage**: crear/modificar/borrar pools, subir ISOs/cloud-images, descargar por URL
  (`/nodes/{n}/storage/{s}/download-url`).
- **Red**: bridges, bonds, VLANs (`/nodes/{n}/network` + apply). **SDN**: zonas, vnets, subnets.
- **Firewall** (datacenter/nodo/VM), **usuarios/tokens/realms/ACL**, **apt del host**,
  **servicios del host**, **power del host** (reboot/shutdown), **syslog**.

### Nivel 2 — Dentro de las VMs: **SÍ, total**
`VM.GuestAgent.Unrestricted` + FileRead/FileWrite → ejecución de comandos como root, push/pull de
archivos, **sin SSH ni túneles**. Es el mecanismo estándar (`lab/pve.py exec|push|pull`).

### Nivel 3 — Shell arbitrario *sobre el host Proxmox*: **NO directo** ← el único límite real
La API REST no expone "ejecutá este comando en el host". Existen `vncshell`/`termproxy` (websocket
interactivo, privilegio disponible) pero no son cómodos de scriptear.

**En la práctica casi no molesta**: red, storage, servicios, apt, firewall y power **tienen endpoint API**.
Lo que quedaría afuera es algo muy puntual (editar un archivo suelto en `/etc` del host, correr un
script ad-hoc). Si aparece esa necesidad **se plantea y se decide juntos** — no se improvisa un
mecanismo de acceso.

---

## 3. Reglas de oro

### Prod
0. **ALCANCE DE ACCESO (regla del operador, 2026-07-28):** el agente opera **únicamente sobre las
   máquinas que él mismo creó**, más el host Proxmox que administra. La red de administración
   `192.168.8.0/24` es **compartida y plana**: hay equipos de terceros ahí. **No se escanea la red,
   no se accede a ninguna IP que no corresponda a una VM propia.** Ante una IP desconocida: no se toca.
0b. **EL AGENTE ES USUARIO DE INFRA, NO TOCA CÓDIGO** (regla del operador, 2026-07-28): en el trabajo
   de infraestructura **no se modifica código del producto**. Si algo del código impide avanzar o
   parece mal, **se plantea y se revisa junto al operador ANTES de cambiar nada**. Leer el código para
   entender: siempre. Cambiarlo por cuenta propia: nunca.
0c. **No inventar mecanismos paralelos a los que el producto ya provee.** Si `add_hive` (o cualquier
   flujo de fluxbee) resuelve algo, se usa **ese** camino, aunque exista un atajo desde la posición
   privilegiada del agente. Motivo: las recetas deben ser **portables** (el próximo prod puede ser
   bare-metal, sin Proxmox ni guest-agent) y deben ejercitar **el camino que está testeado**.
1. **Snapshot antes de todo cambio destructivo o dudoso.** Nombre: `pre-<accion>-YYYYMMDD`.
2. **Un cambio por vez**, verificado antes del siguiente. Nada de lotes a ciegas.
3. **Todo cambio va a la bitácora** del día (§5), incluido el que sale mal.
4. **Lo irreversible se pregunta primero** (§4). Aunque el token pueda hacerlo.
5. **Verificar con evidencia**, no con suposición: si se levantó un servicio, se muestra su estado.
6. **Nada de credenciales de lab en prod** — ver §6.
7. Ante duda entre "probar en prod" y "probar en lab": **se prueba en lab**.

### Lab
Se puede romper. Igual se registra lo que sirve de aprendizaje (los hallazgos son el producto).

---

## 4. Clasificación de cambios

| Tipo | Ejemplos | Requiere aprobación | Snapshot |
|---|---|---|---|
| **Lectura** | inventario, `config-get`, logs, permisos | no | no |
| **Reversible** | crear VM, snapshot, arrancar/parar nodo, `config-set` | no (avisar) | recomendado |
| **Irreversible** | destruir VM, borrar storage pool, cambiar red del host, borrar datos | **SÍ** | **SÍ** |
| **Destructivo de estado** | `cleanall`, dropear DB, reinstalar hive | **SÍ** | **SÍ** |

Regla: *poder hacerlo* (permiso) no es *deber hacerlo*. El token es Administrator; el criterio lo pone el método.

---

## 5. Protocolo de bitácora

Un archivo por día: `lab/logbook/YYYY-MM-DD.md`. Cada cambio, una entrada:

```markdown
## HH:MM — <título corto del cambio>
- **Entorno:** prod (192.168.8.207) | lab (…)
- **Tipo:** lectura | reversible | irreversible | destructivo
- **Objetivo:** por qué se hace (una línea)
- **Ejecutado:** el comando/endpoint EXACTO (reproducible)
- **Resultado:** salida relevante / evidencia
- **Estado:** OK | FALLÓ | REVERTIDO
- **Rollback:** cómo se deshace (o "N/A")
```

**Por qué así:** la bitácora tiene que permitir **reconstruir** el entorno y **auditar** qué pasó.
Es el mismo principio del *command audit log* de SY.admin (`list_recent_commands`): registrar la
mutación con datos suficientes para replicarla. Los secretos **nunca** van en la bitácora — se
registra el *nombre* de la key y su `resource_type`, jamás el valor.

Al final del día: un bloque **Estado al cierre** (qué quedó vivo) y **Pendientes**.

---

## 6. Seguridad operativa

- **Secretos**: van a SY.vault (o al gestor que corresponda). En bitácora/commits/logs solo el
  *nombre* de la key. Los archivos temporales con credenciales se borran (local y remoto) apenas se usan.
- ⚠️ **`lab/template-prep.sh` hornea el usuario `administrator` con password `magicAI`** (default de
  los scripts de `add_hive`). **Está bien para el lab; en prod hay que cambiarlo.** El propio script lo
  advierte. → Para prod: variante con credencial propia y, mejor aún, `ssh_access=key_only_persist`
  (la clave del spoke se revoca tras el join; ver memoria de install-UX).
- **Tokens de Proxmox**: no se pegan en comandos que queden en logs; van por variable de entorno.

---

## 7. Verificación (definición de "listo")

Un cambio no está hecho hasta que hay **evidencia**:
- VM creada → aparece en `list` y el guest-agent responde (`wait-agent`).
- Servicio levantado → `systemctl is-active` lo confirma.
- Nodo fluxbee spawneado → unit activa **y** `CONFIG_GET` devuelve estado esperado
  (`UNCONFIGURED` es correcto hasta cargar el secreto).
- Cambio de red → aplicado **y** conectividad probada (no solo escrito en `interfaces.new`).

Si algo falla: se registra el fallo, se diagnostica, y **el diagnóstico también va a la bitácora**
(los hallazgos son la parte valiosa).

---

## 8. Modelo de deploy Fluxbee (recordatorio, no re-decidir)

- **Solo motherbee se instala por `.deb`.** Worker / ingress / egress son **Linux limpios** que
  motherbee bootstrapea vía `add_hive`. Instalar postgres en un worker = bug.
- **Se compila SIEMPRE en el build box** (fb-build), nunca en un hive ni en prod.
- **Crecer no requiere `.deb` nuevo**: se publica el runtime/paquete y se hace `update`.
- **`base-nodes.json` es la fuente única** de qué entra en el `.deb` (lo leen `build-deb.sh` e `install.sh`).
