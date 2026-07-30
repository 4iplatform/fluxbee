# UPDATE-PLAN — secuencia para poner el update de fluxbee en funcionamiento

> **Por qué existe este documento.** Decisión del operador (2026-07-30): *"para resolver los problemas
> en prod sería mejor tener el update andando bien, y después ir por los bugs"*. Sin update operable,
> cada arreglo exige reinstalar desde cero — y eso no escala ni siquiera para una prod de prueba.
>
> **Documentos hermanos:** [`HANDBOOK.md`](HANDBOOK.md) (instalar desde cero, ya validado) ·
> [`PENDING-BUGS.md`](PENDING-BUGS.md) (los hallazgos, serie `U-*`) · [`METHOD.md`](METHOD.md).

---

## 1. El encuadre: minor y major

Decisión del operador, y acota el problema de golpe:

| | qué es | cómo se resuelve | estado |
|---|---|---|---|
| **major** | cambio grande, o algo se rompió | **instalar desde cero con el `.deb`** | ✅ validado, receta en el HANDBOOK |
| **minor** | cambio chico que no debería romper prod | **update en caliente** | ❌ es lo que hay que hacer funcionar |

**La consecuencia buena:** si los majors se resuelven reinstalando, **el sistema de update solo tiene
que hacer bien los minors.** No necesita migraciones complejas, ni rollback perfecto, ni compatibilidad
entre versiones lejanas. Es un problema mucho más chico que "un sistema de actualización completo".

**El principio que lo gobierna** (operador, textual): *"nada es importante para mantenerlo, solo el
trabajo de tener que hacerlo de vuelta. Ese trabajo no vale la pena para dejar cosas raras en el
diseño de fluxbee."* El DNS y el certificado de prod son de prueba. **Evitar rehacer trabajo no es
razón para torcer el diseño.**

**El rollback, entonces, es externo y simple:** snapshot de las VMs. No hace falta que fluxbee tenga
un rollback perfecto para que esto avance.

### La receta de snapshot — en frío, y por qué

Los snapshots de Proxmox son **por-VM: no hay grupo de consistencia.** fluxbee tiene estado en las
cuatro máquinas (Postgres + vault en motherbee; `hive.yaml`, TLS y clave HMAC de identidad en cada
spoke). Cuatro snapshots en caliente tomados con milisegundos de diferencia pueden dejar, al
rollbackear, un estado **desgarrado**: Postgres recordando un join que el disco del spoke no, o claves
HMAC divergentes que rompen la autenticación del mesh. Un snapshot con RAM además captura la réplica
de identidad en SHM, que puede discrepar del disco al volver.

**Con las VMs apagadas el problema desaparece por construcción**, y el downtime es gratis porque prod
no le sirve a nadie:

```bash
# 1. apagar: spokes primero, motherbee al final
for v in 101 102 103 100; do POST /nodes/pve/qemu/$v/status/shutdown; done   # esperar cada task

# 2. snapshot de las 4 (en frio, SIN vmstate)
for v in 100 101 102 103; do
  POST /nodes/pve/qemu/$v/snapshot  snapname=pre-update-0.1.1  description="..."
done

# 3. arrancar: motherbee primero
for v in 100 101 102 103; do POST /nodes/pve/qemu/$v/status/start; done

# rollback, si hace falta: apagar las 4 -> rollback las 4 -> arrancar
```

**Espacio:** `local-lvm` es LVM-thin (soporta snapshots), con **156 G libres** de 186 G y solo 29.8 G
realmente usados. Sobra.

**El segundo rollback, complementario:** conservar el `.deb` **anterior** publicado en el repo apt de
`fb-build`, para poder bajar de versión con `apt`. Hoy es el único downgrade posible del producto,
porque no existe un comando de rollback de core ([U-5](PENDING-BUGS.md#u-5)).

---

## 2. Lo que ya está construido (y funciona mejor de lo esperado)

Vale decirlo antes de la lista de problemas: **no falta maquinaria.**

```
build .deb          ->  apt install en MOTHERBEE  ->  syncthing replica dist/  ->  POST /hives/{h}/update
  [publicacion]           [instalacion local]            [transporte]                 [aplicacion, por hive]
```

- `build-deb.sh` hornea cada binario **dos veces**: `/usr/bin/<c>` y `dist/core/bin/<c>`, más un
  `dist/core/manifest.json` con **sha256 y tamaño reales** de build-time.
- El transporte es **syncthing** (folder `fluxbee-dist`, `sendonly` en MB / `receiveonly` en el spoke),
  y se puede forzar/esperar con `POST /hives/{h}/sync-hint {channel:"dist"}`.
- La aplicación es **local en el destino**: diff por sha → backup de los binarios previos → copia con
  staging + `rename()` **atómico** → regeneración de units → health gate → rollback local ante fallo.
  El orchestrator se auto-reinicia diferido para no cortarse a sí mismo.
- Gate de *staleness* por hash: si el hash local no coincide con el pedido, responde `202 sync_pending`
  en vez de aplicar algo a medias.
- Para **runtimes** hay incluso fanout: `publish_runtime_package` con `sync_to[]` / `update_to[]`,
  semver estricto, e instalación **idempotente byte a byte**.

El problema son **dos defectos puntuales que vuelven todo esto inoperable** — y los dos son
silenciosos.

---

## 3. Los dos bloqueantes

### 🔴 [U-1](PENDING-BUGS.md#u-1) — el update reporta éxito sin reiniciar nada

`restart_local_core_services_with_health_gate` llama **`systemctl start`** sobre servicios que ya están
corriendo: no-op, systemd devuelve éxito, el proceso sigue con el binario viejo. Y el nombre se apila
en la lista `restarted` que devuelve la API. **No existe `systemd_restart` en el archivo**; las rutas
remotas sí usan `systemctl restart` → **la ruta local es la divergente.**

Es el peor tipo de fallo: los binarios *sí* quedan cambiados en disco, así que el update "funciona"…
en el próximo reboot. Se puede correr, ver `ok`, reiniciar la máquina días después por otro motivo, y
concluir que el mecanismo anda bien.

**Es bloqueante en el sentido literal: no se puede probar un mecanismo cuya señal de éxito es falsa.**

### 🔴 [U-2](PENDING-BUGS.md#u-2) — ingress y egress no tienen carpeta de dist

`dist.sync.enabled: false` **hardcodeado** en los `format!` del `hive.yaml` de esos dos roles
(orchestrator:18211 y :18839), mientras el worker lo tiene parametrizado (:17440). Verificado en las
máquinas: el worker tiene `fluxbee-dist` en `receiveonly`, el ingress **no la tiene**.

Pero syncthing **ya corre en el ingress y ya está emparejado** — recibe `fluxbee-blob-public` en
receive-only. Falta compartirle la carpeta, y **eso no choca con la invariante P5 del DMZ**, que es
específica de `blob/active`.

Importa porque `sy_edge` —la puerta pública— vive justo ahí.

---

## 4. La secuencia

**Regla:** prod solo ve un `.deb` que ya pasó las fases de dev. Y **primero medir, después arreglar**,
para que quede evidencia del bug en vez de solo el arreglo.

### FASE 0 — Hacer el update *medible* (dev, sin tocar prod)

1. **Escribir el assert que falta**, que hoy tiene que **fallar**:
   ```bash
   p=$(systemctl show -p MainPID --value sy-config-routes)
   readlink /proc/$p/exe    # "(deleted)" => corre el binario viejo
   systemctl show -p ActiveEnterTimestamp --value sy-config-routes
   sha256sum /usr/bin/sy-config-routes   # vs dist/core/manifest.json
   ```
   Que falle **es** la prueba de U-1. Sin este assert no hay forma de distinguir un update real de uno
   fantasma.
2. **Arreglar U-1** (una línea, alineando con la ruta remota). *A charlar antes:* `restart` vs
   `stop`+`start` cambia la ventana de indisponibilidad del spoke.
3. **Arreglar U-2** (parametrizar los dos `format!` + peering de la carpeta). *A charlar:* si el default
   para esos roles es `true`, y **cómo se le agrega la carpeta a las cajas ya unidas** sin re-hacer el
   join.

### FASE 1 — dev: upgrade del `.deb` en motherbee

Snapshot en frío primero. Antes del upgrade, **plantar la trampa de [U-3](PENDING-BUGS.md#u-3)**:
publicar un runtime en caliente y verificar que aparece en el manifest.

**Éxito:** 19 nodos `active`, 0 `failed`, `GET /versions` con el hash nuevo, el admin responde.
**Se espera que falle** (y hay que documentarlo): el runtime publicado en caliente desapareció del
manifest → U-3 confirmado en vivo.

### FASE 2 — dev: `category=core` al worker

```bash
curl -sS $H/hives/motherbee/versions            # hash nuevo (origen)
curl -sS -X POST $H/hives/worker1/sync-hint -d '{"channel":"dist","wait_for_idle":true}'
curl -sS $H/hives/worker1/versions              # debe converger
curl -sS -X POST $H/hives/worker1/update -d '{"category":"core","manifest_hash":"<nuevo>"}'
```

**Éxito = `status: ok` Y el assert de FASE 0 en verde** (`exe` no-deleted, `ActiveEnterTimestamp`
posterior, sha == manifest) **Y** el spoke sigue `connected`. Probar también el camino negativo: hash
equivocado → `202 sync_pending`.

### FASE 3 — dev: ingress y egress

Con U-2 arreglado, el mismo `update category=core`. Es la fase que más importa: **es el rol donde vive
la puerta pública**, y el que hoy no tiene camino.

### FASE 4 — dev: paquetes de nodo

Publicar → confirmar que la instancia corriendo **no** cambió (eso es contrato, no bug) → rebindear
`runtime_version` + `restart_node` → verificar binario nuevo **y config preservada**.

### FASE 5 — prod, solo con las fases anteriores en verde

Payload propuesto para `0.1.1`: `98adab7` (recarga del cert TLS) + el fix de U-1 + lo que se resuelva
de U-2. **Los tres son cambios que no tocan protocolo**, lo cual importa por
[U-4](PENDING-BUGS.md#u-4).

Orden: snapshot en frío de las 4 → verificar que el repo apt conserva el `0.1.0` → build en
`fb-build` (~55 min, `nohup`) → `apt-get install` en motherbee → `sync-hint` + `update` al worker con
el assert → ingress/egress → **validación externa del edge** con la prueba estricta sin AIA.

**El criterio de éxito de todo esto** es lindo porque es binario y ya lo tengo hecho: rotar el cert en
el vault y ver si `sy-edge` lo toma **sin que nadie lo reinicie a mano**. Hoy no lo hace. Si después
del update lo hace, el update funcionó de verdad — no porque la API dijo `ok`.

---

## 5. Sobre el lab de dev

**El operador ya lo definió:** *"el dev no tiene que ser fiel, tiene que ser útil. Estamos en
terraforming."* Con eso alcanza — no hace falta que dev reproduzca prod.

**Sirve para** el plano fluxbee puro (§4–§9 del HANDBOOK): tiene los 4 roles (VM240–243) y una VM de
build con repo apt, con acceso por API + guest-agent y snapshots disponibles. Es el lugar correcto para
las fases 0–4.

**No reproduce** el nested-virt sobre VMware, el port-security de ESXi, la SDN con SNAT, la pata
pública real, ni el TLS de Sectigo. Y el hardware es **~8× más rápido** (Rust release 6.5 min vs
55 min) → **ninguna medición de tiempo o timeout del lab traslada a prod.**

Para ejercitar el fix del cert en dev hace falta sembrar **algún** cert en el vault del ingress de dev
— alcanza uno self-signed, porque lo que se prueba es el mecanismo de recarga, no la cadena.

**Desactualizaciones que conviene corregir de paso** (encontradas en la auditoría): `lab/STATUS.md` es
del 2026-06-25 y describe el lab **Docker**, no las VMs; `METHOD.md:24` dice que prod no tiene build box
y trae los binarios del lab, **falso desde el 2026-07-29**; y `docs/packaging-and-build.md:158-164`
lista servidores cuyas VMs se borraron. El riesgo concreto del segundo es que alguien lleve un `.deb`
del lab a prod y se pierda la trazabilidad del commit.

---

## 6. Riesgos, ordenados

1. **Update fantasma (U-1).** Ya explicado: `ok` mentiroso. **Mitigación: no correr un
   `category=core` en prod sin el fix + el assert.**
2. **El ingress de prod hoy no tiene camino de update (U-2).** Y es donde vive el HTTPS público.
3. **Ventana de core caído en motherbee.** El `prerm` para las 17 units sin distinguir upgrade de
   remove; el `postinst` arranca 3 y el resto depende de `bootstrap_local` — que está validado tras un
   **boot**, no tras un **upgrade**. El HTTPS público sobrevive (el edge está en otra máquina), pero
   todo lo que necesite el mesh falla durante la ventana. **Mitigación: snapshot + comparar
   `system_nodes` entre el `hive.yaml.example` nuevo y el `hive.yaml` vivo antes de actualizar.**
4. **Pérdida silenciosa de runtimes publicados en caliente (U-3).** Hoy el daño sería **nulo** (los 6
   runtimes de prod vinieron del `.deb`), pero apenas se publique algo en caliente cada upgrade lo
   borra. **Es un argumento a favor de probar el update ahora, mientras prod es virgen.**
5. **Sin verificación de compatibilidad (U-4) ni rollback de core (U-5).** Mitigación: payload sin
   cambios de protocolo, ventana corta, snapshots, y el `.deb` anterior conservado en el repo.

**Ruido esperado que NO es fallo:** `sy-edge` puede crash-loopear hasta ~9 veces esperando al vault en
un arranque en frío ([PB-4](PENDING-BUGS.md#pb-4)). Esperar ~30 s de convergencia antes de
diagnosticar.

**Lo que no se sabe y hay que medir, no asumir:** cuánto tarda el `apt install` del `.deb` de 240 MB en
el hardware lento de prod, y si el `prerm`/`postinst` chocan con `TimeoutStopSec=15`; si
`bootstrap_local` re-levanta los 19 nodos solo tras un **upgrade**; y si el guest-agent de las VMs
sobrevive el upgrade sin repetir el ciclo A-5.
