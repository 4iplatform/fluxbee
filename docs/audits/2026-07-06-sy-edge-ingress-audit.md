# Auditoria SY.edge / INGRESS - add_ingress, externalize e IO.cloud

Fecha: 2026-07-06  
Repositorio: `/Users/cagostino/code/fluxbee`  
Branch auditada: `daily_onworking_coa`  
Commit auditado: `3fa9646`  
Foco: `docs/edge-ingress-spec-v6.md`, `src/bin/sy_edge.rs`, `src/bin/sy_admin.rs`, `src/bin/sy_orchestrator.rs`, router/policy, vault, identity, packaging y `nodes/io/io-cloud`.

## Resumen ejecutivo

El mecanismo de INGRESS ya tiene piezas importantes implementadas: `SY.edge` existe, se empaqueta, puede bindear un frontend HTTP/HTTPS, persiste rutas por `ich`, rehidrata secrets desde vault por `secret_ref`, forwardea a `owner_l2_name`, y `SY.admin` implementa `externalize`/`unexternalize` con validacion de ownership del ICH. `add_ingress` tambien reconoce el rol `ingress`, copia binarios core y renderiza configuracion remota.

Sin embargo, el sistema todavia no esta listo para exposicion publica segura. El problema principal no esta en el path HTTP basico, sino en los bordes de autoridad y provisioning:

1. **Critico:** `SY.edge` acepta `EDGE_OPEN_URL` y `EDGE_CLOSE_URL` sin validar quien envia el comando, y el router no protege esas acciones.
2. **Alto:** `add_ingress` genera units para `SY.edge` pero no lo habilita/arranca, por lo que puede reportar exito sin listener publico.
3. **Alto:** el unit generado por `add_ingress` no espera `rt-gateway`/`SY.vault`, a diferencia del packaging.
4. **Alto:** TLS es opcional en `add_ingress`, aunque la spec exige `tls_vault_key` para HTTPS ingress.
5. **Alto/Medio:** el alcance de red de `SY.edge` sigue siendo demasiado amplio por la regla general de system nodes en router.

La conclusion operativa es que el "spine" existe, pero faltan cierres de autoridad, arranque y reachability antes de considerar completo el circuito `add_ingress -> externalize -> internet -> IO interno`.

## Alcance y metodologia

Revision estatica de:

- `docs/edge-ingress-spec-v6.md`
- `src/bin/sy_edge.rs`
- `src/bin/sy_admin.rs`
- `src/bin/sy_orchestrator.rs`
- `src/router/mod.rs`
- `src/router/system_policy.rs`
- `src/bin/sy_vault.rs`
- `src/bin/sy_identity.rs`
- `crates/fluxbee_sdk/src/identity.rs`
- `nodes/io/common/src/provision.rs`
- `nodes/io/io-cloud/src/main.rs`
- `scripts/install.sh`
- `packaging/build-deb.sh`
- `config/hive.yaml`
- `packaging/hive.yaml.example`

Check ejecutado:

```bash
cargo test --bin sy_edge
```

Resultado:

- `cargo test --bin sy_edge`: OK, 10 tests pasados.

Limitaciones:

- No se ejecuto un `add_ingress` E2E real sobre Proxmox o host remoto.
- No se hizo prueba con trafico HTTP real atravesando WAN/VPN hacia un nodo IO.
- No se hizo pentest ni fuzzing de payloads `EDGE_OPEN_URL`, registry JSON, headers HTTP o rutas.
- Los hallazgos se derivan de lectura de codigo, spec y tests locales.

## Seguimiento de remediacion

Estado al 2026-07-08 en working tree local, pendiente de commit:

- `EDGE-2026-07-06-01`: Mitigado localmente. `EDGE_OPEN_URL`, `EDGE_CLOSE_URL` y `EDGE_LIST_URLS` quedaron como acciones protegidas; router y `SY.edge` aceptan comandos de servicio solo desde `SY.admin@motherbee`; `SY.edge` valida payloads `open_url` (`ICH` no vacio, owner `IO.*`, family no `system`, secret/ref no vacio para `shared-secret`).
- `EDGE-2026-07-06-02`: Mitigado localmente. `add_ingress` habilita/arranca los system nodes ingress deduplicados, incluye `sy-edge`, espera readiness de `SY.edge` y reporta `edge_service_active`.
- `EDGE-2026-07-06-03`: Mitigado localmente con el diseno correcto para ingress remoto. El unit generado/empaquetado para `sy-edge` depende solo del `rt-gateway` local; el TLS se obtiene desde `SY.vault@motherbee` por la malla. Si TLS fue solicitado y no pudo cargarse, `SY.edge` sale no-cero/fail-closed para que systemd reintente.
- `EDGE-2026-07-06-04`: Mitigado localmente. `add_ingress` exige `ingress.tls_vault_key` por defecto; plaintext requiere `ingress.allow_plaintext=true` y se rechaza en puerto 443.
- `EDGE-2026-07-06-05`: Mitigado en alcance alpha. `SY.edge` ya no hereda el bypass general de system-node ni la excepcion de misma VPN; los comandos de control tienen policy explicita, el path de datos queda limitado a `SY.edge <-> IO.*`, y el acceso a vault queda reducido a `VAULT_GET`/respuesta para TLS y secrets. Queda como mejora futura atarlo a una allowlist por registry si se decide mover esa inteligencia al router.
- `EDGE-2026-07-06-06`: Mitigado. `SY.admin externalize` exige que el ICH exista, tenga owner y este `enabled=true`. Los nodos IO auto-habilitan su propio ICH al arrancar por el flujo canonico `ICH_SET_ENABLED`, y `SY.identity` restringe a los callers `IO.*` a mutar solo su propio ICH.
- `EDGE-2026-07-06-07`: Mitigado localmente. Se agrego `EDGE_LIST_URLS` en `SY.edge` y `list_externalized` en `SY.admin`, con filtrado para que un `IO.*` vea solo sus canales.
- `EDGE-2026-07-06-08`: Mitigado localmente. `add_ingress` ya no acepta seed normal de endpoints; rechaza `ingress.endpoints_json` no vacio y escribe un registry vacio.
- `EDGE-2026-07-06-09`: Parcial. `IO.cloud` sigue siendo stub/simple de producto, pero el circuito alpha quedo operativo: asegura y habilita su ICH, reintenta `externalize` hasta que `SY.edge@ingress1` exista, y responde requests edge-forwarded para smoke/E2E.

Validacion local agregada:

- `cargo test --bin sy_edge --bin sy_admin --bin sy_orchestrator`: OK.
- `cargo test -p json-router edge_`: OK.
- `cargo test -p json-router system_policy`: OK.
- `cargo test --manifest-path nodes/io/Cargo.toml -p io-cloud`: OK.
- `cargo test --manifest-path nodes/io/Cargo.toml -p io-common provision::tests`: OK.
- `cargo test --bin sy_identity ich_enabled`: OK.
- `cargo test --bin sy_identity io_callers`: OK.

Validacion lab Proxmox ejecutada el 2026-07-08:

- Paquete Linux generado en Ubuntu 24.04: `dist/fluxbee_0.1.0-ingressfix3_amd64.deb`, SHA256 `1141d46b014a76a0a7f8952ef8ee522d5ece1abe074954d9d647e45ce7819c30`.
- Instalado en VM 201 `fb-mb` y VM 202 `fb-ing`; ambas quedaron en version `0.1.0-ingressfix3`.
- Motherbee: 0 failed units; `rt-gateway`, `sy-config-routes`, `sy-admin`, `sy-identity`, `sy-vault`, `sy-orchestrator` e `io-cloud` activos.
- Ingress: 0 failed units; `rt-gateway`, `sy-config-routes`, `sy-edge` y `sy-orchestrator` activos; `SY.vault`/`SY.admin`/`SY.identity`/`IO.cloud` inactivos localmente como corresponde al rol ingress.
- `SY.edge@ingress1` obtuvo TLS desde `SY.vault@motherbee`, escucho en `0.0.0.0:443`, y persistio `/etc/fluxbee/edge.endpoints.json` con `ich:14b66389-d425-531c-a140-a591d25e8f39 -> IO.cloud@motherbee`.
- `IO.cloud@motherbee` auto-habilito su ICH, reintento `externalize` mientras `SY.edge@ingress1` no existia y publico correctamente en el intento 7 cuando el edge conecto.
- `SY.admin@motherbee` registro `externalize: edge opened URL (ACKed)` para `SY.edge@ingress1`; no hubo `UNREACHABLE` nuevo despues del fix de routing de responses.
- Prueba HTTPS desde la red de las VMs: `curl -k https://192.168.103.151/e/ich:14b66389-d425-531c-a140-a591d25e8f39` devolvio `HTTP/1.1 200 OK` con `handled_by:"IO.cloud@motherbee"`.
- La prueba desde el host macOS hacia `192.168.103.151:443` fallo por ruta host->VM (`No route to host`); no es falla del ingress, ya que VM->ingress y localhost->edge respondieron 200.

## Hallazgos

### EDGE-2026-07-06-01 - Critico - `SY.edge` acepta abrir/cerrar URLs sin validar autoridad

Estado: Mitigado y validado en lab

Evidencia:

- El loop de `SY.edge` procesa `MSG_EDGE_OPEN_URL` y `MSG_EDGE_CLOSE_URL` directamente antes de otras acciones de sistema (`src/bin/sy_edge.rs:296-335`).
- `apply_open_url` parsea el payload como `EndpointRow`, resuelve secret, inserta en el registry, persiste y responde, pero no valida `req.routing.src_l2_name`, `req.routing.src`, hive ni autoridad del caller (`src/bin/sy_edge.rs:375-405`).
- `apply_close_url` remueve la entrada por `ich` y persiste sin validar origen (`src/bin/sy_edge.rs:420-455`).
- La tabla `PROTECTED_SYSTEM_ACTIONS` no incluye `EDGE_OPEN_URL` ni `EDGE_CLOSE_URL` (`src/router/system_policy.rs:25-44`).
- El router permite trafico VPN si `src` o `dst` es system node (`src/router/mod.rs:5198-5211`), y `SY.edge` califica como system node por prefijo `SY.` (`src/router/mod.rs:5183-5185`).
- La spec dice que publicar endpoint debe pasar por `SY.admin`/autoridad, y que `EDGE_OPEN_URL` es un comando de servicio emitido al edge luego de resolver ownership (`docs/edge-ingress-spec-v6.md:122-189`).

Impacto:

Un nodo interno que pueda rutear a `SY.edge` puede publicar o cerrar rutas publicas arbitrarias. En el caso mas grave puede registrar un `ich` propio o ajeno, elegir `owner_l2_name`, poner `auth_mode = "public"` y exponer un handler sin pasar por la autorizacion de `SY.admin`. Tambien puede hacer DoS logico cerrando rutas existentes.

Recomendacion:

- Agregar `MSG_EDGE_OPEN_URL` y `MSG_EDGE_CLOSE_URL` a una politica protegida de router, con origenes permitidos explicitos.
- Agregar defensa local en `SY.edge`: rechazar comandos mutantes si `src_l2_name` no corresponde a una autoridad esperada.
- Definir la autoridad exacta: probablemente `SY.admin@motherbee` para externalize de IO owners, y solo si se necesita, `SY.orchestrator@<ingress_hive>` para operaciones internas de provisioning.
- Validar invariantes de payload en edge aunque el caller sea autorizado: `owner_l2_name` debe ser `IO.*`, `ich` no vacio, `auth_mode` soportado, `inbound_family` no system/protegido.
- Agregar tests negativos que invoquen `EDGE_OPEN_URL` desde un `IO.*` no autorizado y desde un `SY.*` no permitido.

### EDGE-2026-07-06-02 - Alto - `add_ingress` no habilita ni arranca `sy-edge`

Estado: Mitigado y validado en lab

Evidencia:

- `add_ingress_hive_flow` resuelve system nodes de rol ingress y genera units para esos nodos (`src/bin/sy_orchestrator.rs:17827-17849`).
- El loop que habilita/reinicia servicios remotos solo incluye `rt-gateway` y `sy-orchestrator` (`src/bin/sy_orchestrator.rs:17858-17874`).
- El wait posterior solo verifica `sy-orchestrator` remoto (`src/bin/sy_orchestrator.rs:17875-17890`).
- `name_to_service` mapea `SY.edge` a `sy-edge`, por lo que existe forma local de derivar el servicio esperado (`src/bin/sy_orchestrator.rs:3794-3803`).
- La spec de `add_ingress` da por resuelto que el worker ingress instala/ejecuta `SY.edge` como parte del circuito (`docs/edge-ingress-spec-v6.md:344-364`).

Impacto:

`add_ingress` puede terminar exitosamente con WAN/orchestrator conectados, pero sin `SY.edge` corriendo y sin listener publico. Eso rompe el criterio basico de "routear internet a nodos internos" y genera un estado falso positivo dificil de operar.

Recomendacion:

- En `add_ingress`, habilitar y arrancar todos los `ingress_system_nodes` necesarios, al menos `SY.config.routes` y `SY.edge`.
- Agregar health gate especifico para `sy-edge`: systemd active, conexion al router y, si `edge.listen` esta configurado, bind/listener activo.
- Incluir `SY.edge` en la respuesta final de `add_ingress` con flags tipo `edge_service_active`, `edge_frontend_bound`, `edge_tls_loaded`.
- Agregar test de render/provisioning que falle si `SY.edge` queda fuera de la lista de servicios enable/start.

### EDGE-2026-07-06-03 - Alto - Unit generado por `add_ingress` no espera router/vault

Estado: Mitigado y validado en lab

Evidencia:

- El unit generico que `add_ingress` escribe para nodos no-orchestrator solo declara `After=network.target` (`src/bin/sy_orchestrator.rs:17839-17844`).
- El unit instalado por `scripts/install.sh` para `sy-edge` debe esperar el `rt-gateway.service` local, pero no un `sy-vault.service` local: en un hive ingress el vault vive en motherbee y se alcanza por la malla.
- El unit del paquete `.deb` debe mantener esa misma semantica (`packaging/build-deb.sh`, `scripts/install.sh`).
- `SY.edge` carga TLS desde vault al startup si hay `tls_vault_key` o cert/key configurados (`src/bin/sy_edge.rs:241-245`).
- Si TLS fue solicitado y no se pudo cargar, `SY.edge` rechaza bindear plaintext y continua sin frontend publico (`src/bin/sy_edge.rs:276-287`).

Impacto:

En un host ingress recien provisionado, `SY.edge` puede arrancar antes de router/vault. Si el vault no responde durante la carga inicial de TLS, el proceso queda vivo pero sin listener. Como no sale con error ni reintenta TLS de forma continua, systemd no lo repara automaticamente.

Recomendacion:

- Reutilizar para `add_ingress` la misma plantilla de unit que packaging usa para `sy-edge`.
- Declarar `After=`/`Wants=` para `rt-gateway.service`; no depender de `sy-vault.service` local en ingress.
- Si TLS fue solicitado y falla la carga desde `SY.vault@motherbee`, salir no-cero para que systemd reinicie, o implementar retry/backoff de fetch TLS antes de bindear.
- Agregar test que compare el unit generado por `add_ingress` con los requisitos minimos de dependencia de `sy-edge`.

### EDGE-2026-07-06-04 - Alto - TLS es opcional para un ingress publico

Estado: Mitigado y validado en lab

Evidencia:

- `resolve_add_hive_ingress_section` exige `ingress.listen`, pero `tls_vault_key` es opcional (`src/bin/sy_orchestrator.rs:204-245`).
- `add_ingress_hive_flow` omite por completo el bloque `edge.tls` cuando no hay `tls_vault_key` (`src/bin/sy_orchestrator.rs:17760-17770`).
- `SY.edge` decide si TLS fue solicitado por config/env; si no fue solicitado, levanta frontend plaintext (`src/bin/sy_edge.rs:241-245`, `src/bin/sy_edge.rs:913-915`).
- La spec marca `tls_vault_key` como condicion para HTTPS ingress y documenta que la ausencia implica plaintext solo como comportamiento tecnico (`docs/edge-ingress-spec-v6.md:356-360`).

Impacto:

Un `add_ingress` puede dejar un servicio publico HTTP sin TLS, incluso si el puerto configurado es 443 o si el operador asumio HTTPS. Para un edge expuesto a internet, esto degrada confidencialidad, autenticidad y compatibilidad con clientes.

Recomendacion:

- Exigir `ingress.tls_vault_key` para `role=ingress` por defecto.
- Permitir plaintext solo con un opt-in explicito de desarrollo, por ejemplo `ingress.allow_plaintext = true`, idealmente rechazado si `listen` usa puerto 443.
- Reflejar el modo final en la respuesta de `add_ingress`.
- Agregar tests de config: `ingress.listen` sin TLS debe fallar salvo opt-in explicito.

### EDGE-2026-07-06-05 - Alto/Medio - `SY.edge` conserva reachability interna demasiado amplia

Estado: Mitigado en alcance alpha y validado en lab

Evidencia:

- La spec dice que el alcance de `SY.edge` debe estar limitado a handler nodes IO y no debe tener reachability blanket como system node (`docs/edge-ingress-spec-v6.md:106-118`).
- El scope alpha todavia lista como pendiente bajar el alcance de reachability del edge (`docs/edge-ingress-spec-v6.md:376-392`).
- `add_ingress` escribe `sy-config-routes.yaml` vacio en el host ingress (`src/bin/sy_orchestrator.rs:17797-17808`), sin allowlist especifica para handlers.
- El router permite rutas entre hives si `src` o `dst` es system node (`src/router/mod.rs:5198-5211`).
- `SY.edge` forwardea requests HTTP construyendo mensajes hacia `owner_l2_name` segun registry (`src/bin/sy_edge.rs:1064-1234`), pero el router no limita al proceso a esos destinos.

Impacto:

Aunque el registry de edge forwardee solo al owner configurado, el proceso `SY.edge` sigue teniendo privilegio de system node a nivel de routing. Si el proceso se compromete por estar expuesto a internet, puede intentar enviar mensajes internos a mas nodos de los que la spec permite.

Recomendacion:

- Sacar a `SY.edge` de la excepcion general de system-node reachability, o agregar una excepcion negativa explicita para edge.
- Introducir una policy de router especifica para `SY.edge`: solo mensajes de datos hacia `owner_l2_name`/familias autorizadas por registry, y solo comandos de control desde autoridad aprobada.
- Considerar que `SY.edge` no tenga permiso para enviar `SYSTEM` protegido nunca, salvo status/config local estrictamente definidos.
- Agregar pruebas de router: `SY.edge -> SY.identity`, `SY.edge -> IO.no-externalizado` y `IO.* -> SY.edge EDGE_OPEN_URL` deben ser rechazados.

### EDGE-2026-07-06-06 - Medio - `externalize` ignora el flag `enabled` del ICH

Estado: Mitigado y validado en lab

Evidencia:

- `ILK_ADD_CHANNEL` crea canales con `enabled: false` por defecto (`src/bin/sy_identity.rs:984-1080`).
- `ensure_own_ich` en IO common devuelve el estado `enabled`, pero no habilita el canal (`nodes/io/common/src/provision.rs:150-235`).
- `IO.cloud` puede llamar a `externalize` inmediatamente despues de asegurar su canal si `IO_CLOUD_EDGE_NODE` esta configurado (`nodes/io/io-cloud/src/main.rs:114-174`).
- `SY.admin` resuelve el owner del ICH, pero no verifica que el canal este enabled antes de emitir `EDGE_OPEN_URL` (`src/bin/sy_admin.rs:3495-3514`).
- `IdentityIchOption` expone el campo `enabled`, y `build_ich_options` lo calcula (`crates/fluxbee_sdk/src/identity.rs:133-142`, `src/bin/sy_identity.rs:1391-1411`).

Impacto:

Un canal creado pero no habilitado puede quedar publicado en internet. Esto contradice el modelo de "safe default" esperado para canales y puede exponer superficies IO antes de completar aprobacion operativa.

Recomendacion:

- En `SY.admin externalize`, exigir que el ICH exista y `enabled == true`, salvo un flag administrativo explicito.
- Hacer que `IO.cloud` pida habilitacion del ICH mediante el flujo canonico antes de externalizar, si esa es la semantica deseada.
- Agregar tests: ICH disabled no puede externalizar; ICH enabled si puede.

### EDGE-2026-07-06-07 - Medio - `list_externalized` esta en scope de spec pero no existe

Estado: Mitigado y validado en lab

Evidencia:

- La spec incluye `list_externalized` en la forma de comandos y en el alpha scope (`docs/edge-ingress-spec-v6.md:151`, `docs/edge-ingress-spec-v6.md:379`).
- La allowlist de acciones admin solo incluye `externalize` y `unexternalize` para este frente (`src/bin/sy_admin.rs:90-104`).
- `dispatch_internal_admin_command` solo dispatcha `externalize` y `unexternalize` (`src/bin/sy_admin.rs:3777-3784`).
- `SY.edge` tiene registry persistido, pero no expone una accion segura para listar rutas externalizadas (`src/bin/sy_edge.rs:713-827`).

Impacto:

No hay una vista autoritativa para reconciliar que esta publicado, auditar ownership, detectar drift o reconstruir estado despues de una reimagen. Operar `externalize`/`unexternalize` sin `list` deja el plano de control incompleto.

Recomendacion:

- Implementar `list_externalized` en `SY.admin` como vista autorizada.
- Definir si la fuente de verdad para listar es identity/core, el registry de edge, o ambas con reconciliacion.
- No exponer secrets; devolver `secret_ref`, `auth_mode`, `methods`, `edge_node`, `ich`, `owner_l2_name`, timestamps y estado de sync si existe.
- Agregar tests de autorizacion: un IO solo ve sus ICH; autoridad interna ve todos segun scope.

### EDGE-2026-07-06-08 - Medio/Baja - `ingress.endpoints_json` permite seed legacy/no validado

Estado: Mitigado y validado en lab

Evidencia:

- `IngressSection` todavia tiene comentarios stale sobre un shape antiguo con `{hash, ilk, handler_node}` y `NODE_CONFIG_SET` (`src/bin/sy_orchestrator.rs:180-202`).
- `add_ingress_hive_flow` acepta `ingress.endpoints_json` opcional y lo escribe directo como `/etc/fluxbee/edge.endpoints.json`; si no existe, escribe `{"endpoints":[]}` (`src/bin/sy_orchestrator.rs:17810-17825`).
- `SY.edge` espera rows con `ich`, `owner_l2_name`, `inbound_family`, `auth_mode`, `secret_ref`, etc. (`src/bin/sy_edge.rs:713-738`).
- Si el archivo falta o no parsea, `SY.edge` arranca con registry vacio (`src/bin/sy_edge.rs:788-810`).
- La spec v6 orienta el modelo a edge nacido vacio y cambios por comandos `EDGE_OPEN_URL`/`EDGE_CLOSE_URL`, no por seed manual de endpoints (`docs/edge-ingress-spec-v6.md:122-189`).

Impacto:

Un seed malformado puede producir 404 silenciosos, drift con la fuente de verdad, o rutas prepublicadas que no pasaron por el flujo de autoridad. El riesgo es menor si `hive.yaml` es operator-owned, pero contradice el modelo de control plane.

Recomendacion:

- Remover `ingress.endpoints_json` del contrato normal de `add_ingress`, o limitarlo a modo dev/migracion.
- Si se conserva, validar contra `EndpointRow` antes de escribir remoto.
- Actualizar comentarios y docs para eliminar el shape legacy.
- Agregar test con payload legacy para confirmar rechazo explicito.

### EDGE-2026-07-06-09 - Baja/Media - `IO.cloud` es un stub y no implementa todavia Fluxbee Cloud real

Estado: Abierto como producto Cloud; smoke ingress validado

Evidencia:

- El componente implementado se llama `IO.cloud`; no hay un nodo `sy.cloud` en el codigo revisado (`nodes/io/io-cloud/src/main.rs:329-343`).
- `IO.cloud` registra/provisiona su ICH y, si `IO_CLOUD_EDGE_NODE` esta seteado, pide `externalize` (`nodes/io/io-cloud/src/main.rs:72-174`).
- El handler actual responde basicamente con echo/metadata de la request (`nodes/io/io-cloud/src/main.rs:292-325`).
- La propia spec ubica `IO.cloud` como nodo IO singleton y deja autenticacion/subject authz de cloud fuera del alpha inmediato (`docs/edge-ingress-spec-v6.md:257-278`, `docs/edge-ingress-spec-v6.md:392`).

Impacto:

El camino sirve para validar la columna vertebral de edge hacia un IO node, pero no equivale a "servir Fluxbee Cloud" en sentido funcional. Falta definir endpoints, subjects, autorizacion de usuarios/tenants, comandos cloud reales y reconciliacion con identity/edge.

Recomendacion:

- Mantener `IO.cloud` como smoke-test de ingress, pero documentarlo explicitamente como stub alpha.
- Separar criterios de aceptacion: "edge path operativo" vs "Cloud product funcional".
- Definir un contrato minimo de Cloud antes de declararlo servido por ingress.

## No regresiones / piezas positivas observadas

- `SY.edge` esta incluido en install local y paquete `.deb` (`scripts/install.sh:441`, `scripts/install.sh:462`, `packaging/build-deb.sh:19-24`, `packaging/build-deb.sh:52-54`).
- `IO.cloud` se construye e instala como binario/servicio (`scripts/install.sh:247`, `scripts/install.sh:1118-1138`, `packaging/build-deb.sh:36`, `packaging/build-deb.sh:136-157`).
- `config/hive.yaml` y `packaging/hive.yaml.example` ya declaran `system_nodes.ingress = [SY.config.routes, SY.edge]`.
- `SY.admin externalize` valida ownership por `caller_l2_name` para requests originados en nodos IO (`src/bin/sy_admin.rs:3516-3521`, `src/bin/sy_admin.rs:4089-4114`).
- El router estampa `src_l2_name` en mensajes locales y peer, evitando que un caller lo invente directamente (`src/router/mod.rs:973-975`, `src/router/mod.rs:3612-3614`).
- Los tokens de ingress se guardan como `secret_ref` en vault; `SY.edge` no persiste el secreto en claro al escribir registry (`src/bin/sy_admin.rs:3528-3557`, `src/bin/sy_edge.rs:815-827`, `src/bin/sy_edge.rs:989-1037`).
- `SY.edge` filtra headers sensibles/hop-by-hop, aplica limite de body y mapea errores de unreachable/timeout en el frontend (`src/bin/sy_edge.rs:1064-1234`).

## Verificacion ejecutada

```bash
cargo test --bin sy_edge --bin sy_admin --bin sy_orchestrator
cargo test -p json-router edge_
cargo test -p json-router system_policy
cargo test --manifest-path nodes/io/Cargo.toml -p io-cloud
cargo test --manifest-path nodes/io/Cargo.toml -p io-common provision::tests
cargo test --bin sy_identity ich_enabled
cargo test --bin sy_identity io_callers
```

Resultado:

- Todas las pruebas anteriores pasaron.
- Nota: el suite completo `cargo test --manifest-path nodes/io/Cargo.toml -p io-common -p io-cloud` compila, pero tiene fallas preexistentes fuera de este cambio en tests de `inbound` y `io_slack_adapter_config`; las pruebas focales de provisioning/ICH e `io-cloud` pasaron.

## Orden sugerido de remediacion

1. Cerrar autoridad de `EDGE_OPEN_URL`/`EDGE_CLOSE_URL` en router y en `SY.edge`. Es el riesgo mas directo de publicacion no autorizada.
2. Corregir `add_ingress` para habilitar/arrancar `SY.edge` y health-gatear listener/TLS antes de reportar exito.
3. Alinear el unit remoto de `sy-edge` con packaging y resolver el comportamiento ante fallo de TLS/vault.
4. Hacer TLS obligatorio por defecto para ingress publico, con opt-in explicito para plaintext dev.
5. Reducir reachability de `SY.edge` a handlers IO externalizados y familias permitidas.
6. Enforcear `ICH.enabled` en `externalize`.
7. Implementar `list_externalized` y retirar/validar `ingress.endpoints_json`.
