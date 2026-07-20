# Auditoria y cierre E2E de IO.api sobre SY.edge

Fecha: 2026-07-19

Repositorio: `/Users/cagostino/code/fluxbee`

Branch: `daily_onworking_coa`

Base revisada: `cd2ab140c1c5735ecf0898a5f6a28570c90d40bb`

Estado: implementacion terminada y validada en Linux/Proxmox, con una limitacion de topologia Edge
documentada y un problema operacional de upgrade pendiente.

## Decision aplicada

`IO.api` deja de ser un servidor HTTP privado y pasa al modelo Edge-native vigente:

```text
Internet HTTPS
  -> SY.edge POST /e/<ich>
  -> mensaje io.api.inbound.v1 por el router Fluxbee
  -> IO.api instanciado
  -> destino interno fijo configurado por Admin
```

No se agrego un reverse proxy ni una superficie `/api/<ich>`. La autoridad de publicacion sigue
siendo `SY.admin`; el bearer publico es generado por Admin y Edge es la unica frontera HTTP.

Cada instancia de `IO.api`:

- tiene ILK y tenant inyectados por Orchestrator;
- registra un `api_channel_id` propio y obtiene un ICH distinto;
- acepta solo `POST /` y mensajes `io.api.inbound.v1` para su propio ICH;
- valida que el origen router-stamped sea el `SY.edge` configurado;
- usa un `io.dst_node` fijo que el request no puede reemplazar;
- resuelve ILKs de sujeto con coincidencia exacta de tenant;
- no escucha TCP/HTTP ni recibe secretos, API keys o referencias Vault en su config;
- conserva config, ICH y desired state de publicacion entre reinicios;
- soporta `CONFIG_GET` y `CONFIG_SET` por control plane.

Se retiro el codigo legacy de listener Axum, API keys propias, multipart/attachments y webhooks.
Tambien se retiraron specs y runbooks que describian ese contrato obsoleto. La especificacion
activa es `docs/io/io-api-node-spec.md`.

## Integracion de runtime

`io.api` queda como runtime instanciable de primera clase, no como singleton:

- incluido en el paquete Debian y en `dist/runtimes`;
- publicado por el manifest de runtimes;
- materializado por Syncthing en los hives;
- spawneado y administrado por `SY.orchestrator`;
- operable desde los actions/help de `SY.admin` y la documentacion consumida por Archi;
- desplegable mediante `scripts/deploy-io-api.sh`.

El helper de deploy publica el runtime, sincroniza/actualiza el hive, crea o actualiza una
instancia, aplica config tipada y espera el estado de publicacion. Un bearer nuevo se imprime una
sola vez y no se escribe en el log.

## Validacion automatizada en Linux

La fuente local se aplico sobre un checkout Linux limpio en el builder del lab. Resultados:

- `sy-orchestrator`: 127/127 tests;
- `io-api`: 12/12 tests;
- `io-common`: 85/85 tests;
- politica Router/Rego/WASM: 7/7 tests, incluida paridad del WASM embebido;
- `sy-admin` externalize: 2/2 tests filtrados;
- `sy-edge`: 15/15 tests;
- `cargo clippy -p io-api --all-targets --no-deps -- -D warnings`: OK;
- `cargo fmt --all`: OK;
- sintaxis shell de packaging/install/deploy: OK;
- `git diff --check`: OK.

Los unicos warnings observados son `dead_code` preexistentes fuera de `io-api`.

Paquete final construido en Linux:

```text
fluxbee_0.1.0-ioapi-e2e-final4_amd64.deb
size: 223412526 bytes
sha256: 6afb2e3a3a5d01113cb5c58256ecd72dfe073e1c6e6cd96983d1adf9aeb4fc45
```

El mismo paquete final se instalo en motherbee, worker1 e ingress1. Router, Orchestrator,
Admin/Edge y los IO singleton correspondientes quedaron activos, sin unidades fallidas. El smoke
final uso el runtime `0.1.0-ioapi-e2e-final4` incluido en ese paquete y reconfirmo POST valido,
bearer invalido, ausencia de secretos en `CONFIG_GET`, ausencia de listener, handoff de identidad
por Frontdesk y unpublish 404. El binario IO.api instalado en los tres hives dio el mismo SHA-256:
`27d363e3d0c6fbf6cf4c7a917df839015416923b624d6774aedd965a80ded89d`.

## Validacion E2E en Proxmox

Topologia usada:

| Hive | Rol | VM |
| --- | --- | --- |
| motherbee | control/primary | 201 |
| worker1 | worker | 202 |
| ingress1 | ingress/Edge | 203 |
| builder | build Linux | 210 |

### Publicacion y contrato HTTP

Una instancia en motherbee completo el circuito Edge de punta a punta:

| Caso | Resultado |
| --- | --- |
| `POST /e/<ich>` con bearer valido | HTTP 200, `accepted=true` y `handled_by` correcto |
| bearer invalido | HTTP 401 |
| metodo GET | HTTP 405 |
| override de routing | HTTP 200 con `routing_override_forbidden` |
| path adicional | HTTP 200 con `path_not_found` |
| body de 70 KiB | HTTP 413 `BODY_TOO_LARGE` |
| `explicit_subject by_data` nuevo | HTTP 200; Frontdesk completo el ILK temporal y el request continuo |
| segundo request del mismo sujeto | HTTP 200; reutilizo el mismo ILK completo sin reprovisionar |
| `CONFIG_GET` | no expone bearer ni otra credencial |
| proceso IO.api | conectado solo al router socket, sin listener HTTP |

### Durabilidad y rotacion

- Un restart preservo config, ICH, publicacion Edge y validez del bearer existente.
- `publish=false` retiro la ruta y Edge respondio 404.
- Al reactivar se preservo el ICH, se emitio un bearer nuevo, el anterior respondio 401 y el nuevo
  respondio 200.
- Dos instancias simultaneas obtuvieron ICH y bearer diferentes.
- Cada endpoint fue atendido por su instancia y los bearers cruzados respondieron 401.

### Worker y teardown

Instancias temporales en worker1 se probaron con runtime
`0.1.0-ioapi-e2e-final4` y `publish=false`. Se verifico:

- estado `CONFIGURED` y publicacion `disabled`;
- ICH propio;
- proceso activo por router socket y sin listener HTTP;
- target de Identity, Frontdesk y Admin en los singletons de motherbee;
- kill remoto exitoso;
- eliminacion del directorio persistido y del mapping ILK;
- `ilk_deleted=true` contra `SY.admin@motherbee`;
- purge Vault exitoso contra `SY.admin@motherbee`;
- purge de timers contra `SY.timer@worker1`;
- ausencia final de proceso, directorio e ILK.

El ILK huerfano de una ejecucion previa ya no existia al cierre. Se retiraron la instancia del
worker, el smoke final de motherbee y las identidades humanas temporales generadas por el E2E. No
quedan instancias ni identidades propias de esta prueba; se preservaron los nodos Cloud E2E que ya
existian en el lab.

## Defectos encontrados y corregidos durante E2E

### 1. Autoridad para distribucion cross-hive

La publicacion del runtime llegaba a Admin, pero Router y la validacion de defensa en profundidad de
Orchestrator rechazaban `SYSTEM_UPDATE`/`SYSTEM_SYNC_HINT` cross-hive. Se agregaron esos actions al
conjunto especial autorizado para Admin/Orchestrator en:

- `policy/system.rego` y su `policy/system.wasm` regenerado;
- `src/router/system_policy.rs`.

No se amplio la autoridad de nodos IO ni de un Admin local de worker.

### 2. Ownership del arbol sincronizado

El paquete dejaba `/var/lib/fluxbee/dist` perteneciendo a root mientras Syncthing corre como
`fluxbee`, causando `permission denied`. Se alineo ownership de `dist`, `syncthing` y `blob` en:

- `packaging/deb-postinst`;
- `scripts/install.sh`;
- reconciliacion de `SY.orchestrator` cuando la sincronizacion de dist esta habilitada.

La primera version de la reconciliacion intentaba aplicar el helper recursivo de directorio al
archivo vendor Syncthing y produjo `File exists (os error 17)` en el bootstrap. Se separo el chown
de archivo del chown recursivo de `dist`. El paquete definitivo confirmo en los tres hives:

- ausencia del error de bootstrap;
- vendor Syncthing con ownership `fluxbee:fluxbee`;
- runtime sincronizado, materializado y ejecutable en worker1;
- Syncthing `idle`, con 0 errores, 0 pull errors y 0 items pendientes;
- hashes identicos del manifest, binario IO.api y Orchestrator entre motherbee y worker1.

### 3. Teardown de instancias en worker

El teardown remoto intentaba usar `SY.admin@worker1`, que no existe porque Admin es singleton de
motherbee. La limpieza de Identity y Vault ahora apunta a `SY.admin@motherbee`. El test unitario y
el E2E real confirman el target y la eliminacion completa.

### 4. Handoff Frontdesk y contrato RPC

La primera prueba `explicit_subject by_data` posterior al upgrade devolvio
`invalid_frontdesk_response`. El payload no estaba corrupto: `SY.frontdesk.gov` habia reiniciado
sin config efectiva y respondia su error canonico `type=error/node_not_configured`.

Se cerro el circuito de la siguiente manera:

- Frontdesk se configuro por su `CONFIG_SET` canonico y persistio el estado; una reinstalacion y
  reinicio posterior lo restauraron en `CONFIGURED`;
- IO.api reconoce el payload `type=error` antes de parsear el envelope y lo expone como
  `frontdesk_unavailable` con el codigo original en el detalle;
- el matcher RPC ya no acepta cualquier mensaje correlacionado: solo `msg_type=user` es exito y
  `SYSTEM/UNREACHABLE` o `SYSTEM/TTL_EXCEEDED` son errores terminales;
- el Frontdesk intermedio es configurable mediante `node.frontdesk_target` /
  `IO_API_FRONTDESK_TARGET` y por defecto apunta al singleton `SY.frontdesk.gov@motherbee`, tambien
  cuando IO.api corre en worker.

El E2E final probo un sujeto nuevo, registro completo por Frontdesk, continuacion al destino fijo y
un segundo request que reutilizo exactamente el mismo ILK. El restart de todo el paquete confirmo
que Frontdesk conserva su config efectiva.

## Residuales

### EDGE-H4: routing WAN de un salto

`ingress1` tiene adyacencia directa con motherbee, no con worker1. La implementacion actual de LSA
WAN en Edge resuelve un solo salto, tal como ya documenta `docs/edge-ingress-spec-v6.md` (H4).

Consecuencia: una publicacion cuyo `IO.api` vive en worker1 puede crearse, pero el request desde
Edge termina en HTTP 502 `HANDLER_UNREACHABLE`. El mismo circuito en motherbee funciona completo.
Resolverlo exige una decision de diseno sobre routing multihop o topologia de ingress; no se cambio
ese modelo dentro de este revamp.

### Upgrade de paquete: timeout al detener Orchestrator

En worker1 e ingress1, `apt` espero los 90 segundos de `TimeoutStopUSec` y systemd termino el
Orchestrator con SIGKILL antes de completar el upgrade. El paquete luego se instalo y
Router/Orchestrator arrancaron correctamente. Es un problema de lifecycle de unidades durante
upgrade, no de IO.api, y queda pendiente de una correccion dedicada.

## Conclusion

`IO.api` esta listo como runtime Edge-native instanciable y su circuito completo esta validado en
motherbee. Identidad, aislamiento entre instancias, bearer, configuracion, reinicio, rotacion,
unpublish y teardown quedaron probados en el lab.

La publicacion de una instancia alojada en worker requiere resolver primero EDGE-H4. Hasta entonces,
la ubicacion funcional para endpoints publicos es un hive directamente alcanzable por el ingress,
como motherbee en la topologia probada.
