# DEPLOYMENTS — registro de versiones desplegadas en PROD (auditoría)

> Ledger **append-only** de cada versión que TOCA producción (`pve` @ `192.168.8.207`). Es el
> registro de auditoría: **qué** versión, **cuándo**, **qué commit**, **qué cambió**, **cómo se
> verificó** y **cómo se revierte**. Una fila en la tabla + una entrada detallada por versión.
>
> **Regla (HANDBOOK §12): ningún `apt install` en prod — ni core-update a un spoke — sin una
> entrada acá.** Sin entrada, el deploy no está hecho.
>
> **Snapshots: nunca más de 3 por VM (HANDBOOK §12, desde 2026-09-25).** El rollback "por snapshot"
> de las entradas viejas puede ya no existir: el 2026-09-25 se borraron los de 0.1.19 a 0.1.29.
> El camino que siempre queda es `apt install fluxbee=<anterior>`, porque el repo conserva todas
> las versiones.
>
> Hermanos: [`logbook/HANDBOOK.md`](logbook/HANDBOOK.md) (recetas de deploy), [`logbook/METHOD.md`](logbook/METHOD.md)
> (cómo se opera la infra), `logbook/YYYY-MM-DD.md` (bitácora narrativa), [`logbook/FINDINGS.md`](logbook/FINDINGS.md)
> (hallazgos/bugs). Este doc es SÓLO el ledger de versiones en prod. Fechas en **ART (−03)**;
> el journal del host prod va en EDT (ver METHOD §1).

## Tabla rápida (auditoría)

| Versión | Fecha (ART) | Commit | Alcance | Estado | Rollback |
|---|---|---|---|---|---|
| **0.1.78** | 2026-10-17 | `104fa65` | motherbee + spokes (core) | ✅ live | snap `pre-stream2b-0-1-78` (las 4 VMs) · `apt install fluxbee=0.1.77` + core-update de los spokes |
| **0.1.77** | 2026-10-17 | `377945a` | motherbee + spokes (core) | ✅ reemplazado por 0.1.78 | snap `pre-stream2a-0-1-77` (las 4 VMs); `apt` solo no alcanza (ver la entrada) |
| **0.1.76** | 2026-10-16 | `e24e7d3` | motherbee + spokes (core) | ✅ reemplazado por 0.1.77 | snap `pre-stream1b-0-1-76` (las 4 VMs); `apt install fluxbee=0.1.75` pierde la inbox purgada (ver la entrada) |
| **0.1.75** | 2026-10-16 | `1c00389` | motherbee + spokes (core) | ✅ reemplazado por 0.1.76 | snap `pre-stream1a-0-1-75` (las 4 VMs); `apt` solo no alcanza (ver la entrada) |
| **0.1.74** | 2026-10-16 | `0d69329` | motherbee + spokes (core) | ✅ reemplazado por 0.1.75 | snap `pre-sdk-0-1-74` (las 4 VMs) · `apt install fluxbee=0.1.73` |
| **0.1.73** | 2026-10-16 | `822f237` | motherbee + spokes (core) + `hive.yaml` (IA: Claude) | ✅ reemplazado por 0.1.74 | snap `pre-cognition-0-1-73` (las 4 VMs) · `apt install fluxbee=0.1.72` · `hive.yaml.pre-claude-0-1-72` |
| **0.1.72** | 2026-10-16 | `17e1edd` | motherbee + spokes (core) | ✅ reemplazado por 0.1.73 | snap `pre-sync-0-1-72` (las 4 VMs) · `apt install fluxbee=0.1.71` |
| **0.1.71** | 2026-10-16 | `73f7132` | motherbee + spokes (core) | ✅ reemplazado por 0.1.72 | snap `pre-gate-0-1-71` (las 4 VMs) · `apt install fluxbee=0.1.70` |
| **0.1.70** | 2026-10-16 | `c289bb3` | motherbee + spokes (core) | ✅ reemplazado por 0.1.71 | snap `pre-wf-0-1-70` (las 4 VMs) · `apt install fluxbee=0.1.69` |
| **0.1.69** | 2026-10-15 | `6f03310` | motherbee + spokes (core) | ✅ reemplazado por 0.1.70 | snap `pre-teardown-0-1-69` (las 4 VMs) · `apt install fluxbee=0.1.68` |
| **0.1.68** | 2026-10-15 | `1b47c3e` | motherbee + spokes (core) | ✅ reemplazado por 0.1.69 | snap `pre-cloud-0-1-68` (las 4 VMs) · `apt install fluxbee=0.1.67` |
| **0.1.67** | 2026-10-15 | `e85041d` | motherbee + spokes (core) | ✅ live | snap `pre-tenant-0-1-67` (las 4 VMs) · `apt install fluxbee=0.1.66` (la migración v4 no se deshace; ver la entrada) |
| **0.1.66** | 2026-10-14 | `96c8d77` | motherbee + spokes (core) | ✅ live | snap `pre-secrets-0-1-66` (las 4 VMs) · `apt install fluxbee=0.1.65` |
| **0.1.65** | 2026-10-14 | `8b3e951` | motherbee + spokes (core) | ✅ live | snap `pre-scope-0-1-65` (las 4 VMs) · `apt install fluxbee=0.1.64` (la migración v3 no se deshace; ver la entrada) |
| **0.1.64** | 2026-10-13 | `f8c73c5` | motherbee + spokes (core) | ✅ live | snap `pre-cloud-0-1-64` (las 4 VMs) · `apt install fluxbee=0.1.63` |
| **0.1.63** | 2026-10-06 | `aab0e3e` | motherbee + spokes (core) | ✅ live | snap `pre-a58-0-1-63` (las 4 VMs) · `apt install fluxbee=0.1.62` (HANDBOOK, rollback bajo 0.1.63) |
| **0.1.62** | 2026-10-06 | `b4868e8` | motherbee + spokes (core) | ✅ live | snap `pre-identity-0-1-62` (las 4 VMs) · `apt install fluxbee=0.1.61` (spokes primero) |
| **0.1.61** | 2026-10-06 | `e0f2d04` | motherbee + spokes (core) | ✅ live | snap `pre-ssh-0-1-61` (las 4 VMs) · `apt install fluxbee=0.1.60` |
| **0.1.60** | 2026-10-06 | `0af5469` | motherbee + spokes (core) | ✅ live | snap `pre-hardening-0-1-60` (las 4 VMs) · `apt install fluxbee=0.1.59` |
| **0.1.59** | 2026-10-06 | `f60eb92` | motherbee + spokes (core) | ✅ live | snap `pre-stage3-0-1-59` (las 4 VMs) · `apt install fluxbee=0.1.58` |
| **0.1.58** | 2026-10-06 | `9d9391b` | motherbee + spokes (core) | ✅ live | snap `pre-stage2-0-1-58` (las 4 VMs) · `apt install fluxbee=0.1.57` |
| **0.1.57** | 2026-10-06 | `a703b89` | motherbee + spokes (core) | ✅ live | snap `pre-posture-0-1-57` (las 4 VMs) · `apt install fluxbee=0.1.56` |
| **0.1.56** | 2026-10-03 | `715143a` | motherbee + spokes (core) | ✅ live | snap `pre-root-io-0-1-56` (las 4 VMs) · `apt install fluxbee=0.1.55` |
| **0.1.55** | 2026-10-02 | `975fd34` | motherbee + spokes (core) | ✅ live | snap `pre-root-tenant-0-1-55` (las 4 VMs) · `apt install fluxbee=0.1.54` |
| **0.1.54** | 2026-10-02 | `6aa4ee4` | motherbee + spokes (core) | ✅ live | snap `pre-gov-0-1-54` (las 4 VMs) · `apt install fluxbee=0.1.53` |
| **0.1.53** | 2026-10-02 | `26edcd5` | motherbee + spokes (core) | ✅ live | snap `pre-ai-generic-0-1-53` (las 4 VMs) · `apt install fluxbee=0.1.52` |
| **0.1.52** | 2026-10-02 | `0670184` | motherbee + spokes (core) | ✅ live | snap `pre-cleanup-0-1-52` (las 4 VMs) · `apt install fluxbee=0.1.51` |
| **0.1.51** | 2026-10-01 | `f7c2520` | motherbee + spokes (core) | ✅ live | snap `pre-query-decode-0-1-51` (las 4 VMs) · `apt install fluxbee=0.1.50` |
| **0.1.50** | 2026-10-01 | `aa206ac` | motherbee + spokes (core) | ✅ live | snap `pre-config-routes-0-1-50` (las 4 VMs) · `apt install fluxbee=0.1.49` |
| **0.1.49** | 2026-10-01 | `c921d1a` | motherbee + spokes (core) | ✅ live | snap `pre-architect-fixes-0-1-49` (las 4 VMs) · `apt install fluxbee=0.1.48` |
| **0.1.48** | 2026-10-01 | `1486807` | motherbee + spokes (core) | ✅ live | snap `pre-architect-opa-0-1-48` (las 4 VMs) · `apt install fluxbee=0.1.47` |
| **0.1.47** | 2026-10-01 | `652a9c9` | motherbee + spokes (core) | ✅ live | snap `pre-opa-blockB-0-1-47` (las 4 VMs) · `apt install fluxbee=0.1.46` |
| **0.1.46** | 2026-10-01 | `7371ca1` | motherbee + spokes (core) | ✅ live | snap `pre-opa-blockA-0-1-46` (las 4 VMs) · `apt install fluxbee=0.1.45` |
| **0.1.45** | 2026-10-01 | `d6505ff` | motherbee + spokes (core) | ✅ live | snap `pre-rule3-0-1-45` (las 4 VMs) · `apt install fluxbee=0.1.44` |
| **0.1.44** | 2026-10-01 | `005d0ad` | motherbee + spokes (core) | ✅ live | snap `pre-src-binding-0-1-44` (las 4 VMs) · `apt install fluxbee=0.1.43` |
| **0.1.43** | 2026-09-30 | `2b3abb2` | motherbee + spokes (core) | ✅ live | snap `pre-sync-hint-0-1-43` (las 4 VMs) · `apt install fluxbee=0.1.42` |
| **0.1.42** | 2026-09-30 | `ac301d9` | motherbee + spokes (core) | ⚠️ superada (escaneo rechazado: 0.1.43) | snap `pre-opa-fixes-0-1-42` (las 4 VMs) · `apt install fluxbee=0.1.41` |
| **0.1.41** | 2026-09-30 | `72891be` | motherbee + spokes (core) + `hive.yaml` | ✅ live | snap `pre-opa-global-0-1-41` (las 4 VMs) · `apt install fluxbee=0.1.40` + `hive.yaml.pre-opa-0-1-41` |
| **0.1.40** | 2026-09-30 | `43dd174` | motherbee + spokes (core) | ✅ live | snap `pre-purge-order-0-1-40` (las 4 VMs) · `apt install fluxbee=0.1.39` |
| **0.1.39** | 2026-09-30 | `85c3244` | motherbee + spokes (core) | ✅ live | snap `pre-identity-baseline-0-1-39` (las 4 VMs) · `apt install fluxbee=0.1.38` |
| **0.1.38** | 2026-09-30 | `20baec3` | motherbee + spokes (core) | ✅ live | snap `pre-wf-mirror-0-1-38` (las 4 VMs) · `apt install fluxbee=0.1.37` |
| **0.1.37** | 2026-09-30 | `c09f9af` | motherbee + spokes (core) | ✅ live | snap `pre-followups-0-1-37` (las 4 VMs) · `apt install fluxbee=0.1.36` |
| **0.1.36** | 2026-09-30 | `d24e3aa` | motherbee + spokes (core) | ✅ live | snap `pre-config-scope-0-1-36` (las 4 VMs) · `apt install fluxbee=0.1.35` |
| **0.1.35** | 2026-09-28 | `5f1cd71` | motherbee | ✅ live | snap `pre-teardown-fix-0-1-35` · `apt install fluxbee=0.1.34` |
| **0.1.34** | 2026-09-28 | `60331c5` | motherbee + spokes (core) | ⚠️ superada (regresión teardown) | snap `pre-lifecycle-0-1-34` (las 4 VMs) · `apt install fluxbee=0.1.33` |
| **0.1.33** | 2026-08-28 | `01b6266` | motherbee | ✅ live | `apt install fluxbee=0.1.32` |
| **0.1.32** | 2026-08-27 | `90fd64a` | motherbee | ✅ live | `apt install fluxbee=0.1.31` |
| **0.1.31** | 2026-08-26 | `862c5e4` | motherbee | ✅ live | snap `pre-rpc-poison-fix-0-1-31` · `apt install fluxbee=0.1.30` |
| **0.1.30** | 2026-08-26 | `af1166a` | motherbee | ✅ live | snap `pre-liveness-fix-0-1-30` · `apt install fluxbee=0.1.29` |
| **0.1.29** | 2026-08-25 | `a0bc25d` | motherbee | ✅ live | snap `pre-cloud-readpath-0-1-29` · `apt install fluxbee=0.1.28` |
| **0.1.28** | 2026-08-25 | `1438737` | motherbee | ✅ live | snap `pre-frontdesk-configplane-0-1-28` · `apt install fluxbee=0.1.27` |
| **0.1.27** | 2026-08-25 | `000de90` | motherbee | ✅ live | snap `pre-frontdesk-autonomous-0-1-27` · `apt install fluxbee=0.1.26` |
| **0.1.26** | 2026-08-24 | `cb2d192` | motherbee | ✅ live | snap `pre-frontdesk-handoff-0-1-26` · `apt install fluxbee=0.1.25` |
| **0.1.25** | 2026-08-24 | `69b1c46` | motherbee | ✅ live | snap `pre-observability-0-1-25` · `apt install fluxbee=0.1.24` |
| **0.1.24** | 2026-08-23 | `35349ef` | motherbee | ✅ live | snap `pre-vault-guard-0-1-24` · `apt install fluxbee=0.1.23` |
| **0.1.23** | 2026-08-21 | `4abad1b` | motherbee | ✅ live | snap `pre-cloud-actions-0-1-23` · `apt install fluxbee=0.1.22` |
| **0.1.22** | 2026-08-20 | `15fc77f` | motherbee | ✅ live | snap `pre-frontdesk-0-1-22` · `apt install fluxbee=0.1.21` |
| **0.1.21** | 2026-08-20 | `2f60403` | motherbee | ✅ live | snap `pre-register-human-0-1-21` · `apt install fluxbee=0.1.20` |
| **0.1.20** | 2026-08-20 | `1297ba7` | motherbee | ✅ live | snap `pre-router-0-1-20` · `apt install fluxbee=0.1.19` |
| ≤ 0.1.19 | (pre-ledger) | — | motherbee | histórico, sin registrar | repo apt conserva 0.1.0 … 0.1.19 |

> Este ledger arranca en **0.1.20** (primera vez que se registra formalmente). Las versiones
> anteriores (0.1.0–0.1.19) se desplegaron sin ledger; el repo apt en fb-build las conserva
> (`dpkg-scanpackages -m`) para rollback, pero su detalle vive en la bitácora, no acá.

---

## 0.1.78 — NATS borrado y SY.policy fuera (A-70, etapa 2b)

- **Fecha:** 2026-10-17 (ART) · **Versión anterior:** 0.1.77 · **Commit:** `104fa65`.
  - Diseño: `docs/router-stream-v1.md` §6.3, y cómo quedó construido en §6.9.
  - Una revisión del código con dos miradas (el camino del upgrade, lo que dejó la remoción):
    ningún HIGH. El MEDIUM y los LOW se arreglaron antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **NATS borrado:** se fueron unas 9.000 líneas:
    - el broker del router, el cliente del SDK y `ClientConfig`;
    - los dos diags y ocho scripts;
    - SY.policy;
    - `meta.thread_seq`;
    - la ingesta de `storage.events`, `storage.items` y `storage.reactivation`, que nunca
      tuvieron productor (sus tablas quedan).
  - **SY.storage** no lee nada de NATS:
    - contesta `STORAGE_METRICS_GET` por la malla, un escaneo de la inbox a la vez;
    - `GET /config/storage/metrics` del admin pregunta ahí, con la misma respuesta de antes.
  - **SY.orchestrator:**
    - se une a su router y después espera la base de SY.storage por la malla;
    - mientras tanto los comandos del admin reciben `ORCHESTRATOR_BOOTING`, así nadie queda
      esperando a un hive que no está listo (antes fallaban porque el orquestador no estaba en el
      router todavía).
  - **Transiciones únicas al arrancar, en el log:**
    - los mensajes pendientes del broker por subject;
    - después, `/var/lib/fluxbee/nats` y la sección `nats:` de `hive.yaml`. Esto corre después de
      que el router corre el binario nuevo, porque el viejo, al pararse, vuelca su broker a disco;
    - SY.policy sale de todas las secciones de `hive.yaml`, y se borran su unit y su binario
      donde quedaron.
  - **Los joins** escriben `hive.yaml` sin `nats:`, y nadie crea el directorio `nats`.
  - **SY.cognition** registra de dónde sacó su clave de IA una vez que la buscó.
  - **El chequeo de paridad del catálogo del admin** reconoce `list_streams` y el argumento nuevo
    del deploy de WF. Estaba rojo desde 1b y A-68.
- **Deploy:**
  - build de `104fa65` en 23 min, publish en 13 s, deploy en 241 s;
  - snapshots `pre-stream2b-0-1-78` en las 4 VMs (se borró `pre-stream1a-0-1-75`);
  - 0.1.78 en los 4, health `failed=0 streamlost=0`.
- **Gate:**
  - **Nada de NATS ni de SY.policy en ningún hive:**
    - nada escucha en 4222;
    - no hay `/var/lib/fluxbee/nats`, ni sección `nats:` (tampoco su comentario) ni SY.policy en
      `hive.yaml`;
    - no quedan la unit ni el binario de SY.policy; su unit da `not-found`.
  - **Lo que registró cada orquestador:**
    - **ingress:** "pending={storage.turns: 597}", los turnos del edge que nadie consumía, y
      después borró el directorio;
    - **worker1:** borró la unit y el binario de SY.policy;
    - **motherbee:** sacó SY.policy de su sección `worker` (dos líneas);
    - **los cuatro:** quitaron la sección `nats:`.
  - **Por la malla:**
    - `GET /config/storage/metrics` contesta `ok` (0 pendientes);
    - en el upgrade, el orquestador del motherbee confirmó la base en 10,6 s (18 intentos) y
      después pasó a "hive ready". La espera fue el arranque de SY.storage: el vault le resolvió
      la URL, recibió el broadcast del vault y salió para reconectar (el rescate de siempre).
  - **La ventana de arranque**, con SY.storage retenido 40 s y el orquestador del motherbee
    reiniciado:
    - `GET /hives` contestó `502 TRANSPORT_ERROR` hasta que el orquestador se unió al router;
    - después contestó `ORCHESTRATOR_BOOTING` en 0,01 s;
    - pasó a `ok` 4 s después de liberar SY.storage;
    - nadie quedó colgado.
  - **Regresión de cognition** (un agente en el motherbee, dos turnos desde worker1):
    - la cognition del motherbee procesó los 4 turnos, ninguno sin ILK de agente;
    - una comunicación en la base (turno 4), con el ILK del agente;
    - la región tiene su entrada y los streams entregaron todo.
  - **SY.cognition** registra "looked up its AI key ai_secret_source=vault".
  - **Health:** `failed=0 denials=0 srcdrops=0 streamlost=0` en los 4.
  - **No se probó en vivo:** un join nuevo de un worker con `hive.yaml` sin `nats:`. Necesita una
    VM nueva; el cambio son las tres plantillas, y la revisión lo cubrió.
  - **Limpieza:**
    - el nodo de prueba purgado con su identidad (identity volvió a 28 ILKs);
    - el tenant borrado y purgado;
    - la región vaciada;
    - se borraron 4 turnos, sus 4 filas de inbox, 75 sobres y todas las filas de cognition
      (no queda ninguna);
    - ninguna máscara quedó.
- **Pendiente:** las transiciones (NATS y SY.policy) se borran del código cuando todos los hives
  hayan arrancado 0.1.78, en la pasada de limpieza.
- **Rollback:** snapshot `pre-stream2b-0-1-78`, o `apt install fluxbee=0.1.77` más el
  core-update de los spokes:
  - 0.1.77 arranca sin la sección `nats:`, con los valores por defecto y un broker vacío;
  - SY.policy ya no corría en 0.1.77;
  - lo que no vuelve son los 597 turnos que el broker del ingress tenía sin consumir. Se
    descartaron a propósito (decisión del operador).

## 0.1.77 — Cognition por comunicación: el router del agente le pasa los dos sentidos a su cognition local (A-70, etapa 2a)

- **Fecha:** 2026-10-17 (ART) · **Versión anterior:** 0.1.76 · **Commit:** `377945a`.
  - Diseño: `docs/router-stream-v1.md` §6, conversado con el operador y revisado una vez.
  - Una revisión del código: ningún HIGH. El MEDIUM (una comunicación cargada se quedaba vieja si
    su agente se iba y volvía) y los LOW, arreglados antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **El modelo (decisión del operador):** la memoria es de la comunicación entre ILKs, nunca de
    un hive o un nodo. Una comunicación es `(thread_id, ILK del agente)`, y su dueño es el agente
    que la atiende. El número de turno la ordena; el router ya no asigna `thread_seq`.
  - **Router:**
    - ya no copia turnos a NATS;
    - copia a `cognition.turns` cada turno con `thread_id` que manda un nodo `AI.*` local, o que
      le entrega a uno (local, después de la WAN, después de IRP; nunca copias de tap ni
      broadcasts). El destino es `SY.cognition@<su hive>`, en el motherbee y los workers;
    - adjunta el paquete de memoria por destino, solo a los `AI.*`, el de `(thread_id, nodo
      agente)`; el que trae el emisor se descarta;
    - los lotes del stream llevan una época (`state.json`), para distinguir un log reiniciado de
      repetidos.
  - **SY.cognition:**
    - lee `cognition.turns` del stream de su router; el ILK del agente sale de identity por
      `handler_node` (en caché 60 s);
    - sus sobres (esquema v2, con `agent_ilk` y `turn`) salen como un registro
      `storage.cognition.batch` por turno, por el stream;
    - al arrancar recarga desde SY.storage, por la malla, las comunicaciones que tiene la región
      de memoria (`COGNITION_STATE_GET`, paginado);
    - una comunicación cargada que estuvo 5 min sin turnos se vuelve a pedir antes del próximo, y
      gana el estado guardado si va más adelante;
    - ya no usa NATS ni postgres.
  - **SY.storage:**
    - las tablas de cognition se rearman una vez (marca `cognition_reset_v1`; los datos de prueba
      se tiran, decisión del operador), con la comunicación como clave: `agent_ilk` y
      `source_turn`;
    - una fila solo la reemplaza un turno igual o posterior, también los scopes;
    - expande `storage.cognition.batch` a una fila de inbox por sobre, después de verificar el
      productor;
    - contesta `COGNITION_STATE_GET`, solo a `SY.cognition@*`, en páginas de 64 KB.
  - **SDK de IA:** la respuesta lleva el ILK del agente como `src_ilk` y al interlocutor como
    `dst_ilk`. ai.generic y SY.frontdesk.gov pasan el suyo.
  - **SY.policy se retira (primera de dos versiones):**
    - inactivo y deshabilitado;
    - fuera de la sección propia de cada `hive.yaml` (escritura atómica);
    - el motherbee lo deja en su sección `worker` hasta 2b, porque si no los orquestadores viejos
      de los workers rechazan el core-update.
  - **El broker** borra al arrancar, y rechaza después, los consumidores de turnos de SY.storage,
    SY.cognition y SY.policy.
- **Deploy:**
  - build de `377945a` en 25 min, publish en 14 s, deploy en 230 s;
  - snapshots `pre-stream2a-0-1-77` en las 4 VMs (se borró `pre-sdk-0-1-74`);
  - 0.1.77 en los 4, health `failed=0 streamlost=0`.
- **Gate:**
  - **El upgrade:**
    - SY.policy inactivo y deshabilitado en el motherbee y worker1. El `hive.yaml` del
      motherbee lo nombra solo en la sección `worker`; el de worker1, en ninguna;
    - ningún broker tiene consumidores de `storage.turns`. Los 597 turnos del ingress siguen ahí
      hasta 2b;
    - los streams del motherbee y worker1 suman `SY.cognition@<hive>` como destino; el ingress y
      el egress no;
    - las tablas de cognition quedaron en v2 y vacías;
    - las dos cognitions recargaron por la malla ("ok loaded 0"). La de worker1 antes fallaba
      por no tener base.
  - **Una conversación con el agente en el motherbee** (`AI.gate77@motherbee`, ai.generic 0.1.77,
    en un tenant de prueba) desde un interlocutor de prueba en worker1 (un probe por socket),
    3 turnos:
    - las respuestas salen con el ILK del agente como `src_ilk` y el del interlocutor como
      `dst_ilk`;
    - la cognition del motherbee procesó 6 turnos (3 de ida, 3 de vuelta), ninguno sin ILK de
      agente, con 6 + 6 llamados al modelo (etiquetador y resumidor) y 119 entidades, sin
      errores. La de worker1 no recibió nada;
    - una sola comunicación en la base: la del agente, turno 6, escrita por
      `SY.cognition@motherbee`; todas sus filas llevan el ILK del agente;
    - `turns` tiene las 6 filas, en los dos sentidos;
    - la región de memoria tiene la entrada con el nodo y el ILK del agente;
    - el agente no tenía clave en su tenant (desde 0.1.66 no hay caída a la clave raíz), así que
      contestó con el error `missing_ai_api_key`. Cognition evaluó esas respuestas con la clave
      del core. El gate no depende de lo que contesta el agente, y no se copió la clave del
      operador a un tenant de prueba.
  - **El paquete:** con el nodo real detenido, un probe por socket tomó su nombre, y el
    interlocutor le mandó un turno con un paquete falso. El probe recibió el paquete real de su
    comunicación (contextos, una memoria, razones) y no el falso.
  - **La recarga:**
    - `sy-cognition` del motherbee reiniciado: recargó por la malla en 0,3 s ("loaded 1, without
      state 0, failed 0") y la entrada de la región quedó;
    - el turno siguiente siguió en los turnos 9 y 10, sobre las mismas filas.
  - **El refetch por inactividad:**
    - un turno dentro de los 5 min no pidió nada;
    - después de 5,5 min sin turnos, el siguiente pidió el estado una vez y siguió en el turno 14.
  - **Un agente en worker1** (`AI.gate77w@worker1`), con 2 turnos desde el motherbee:
    - la cognition de worker1 procesó 4 y sus 80 entidades llegaron a postgres, escritas por
      `SY.cognition@worker1` con el ILK de ese agente;
    - la del motherbee no las tocó;
    - cada región tiene solo la comunicación de su propio agente.
  - **Los streams** entregaron todo una vez (cursores iguales, nada perdido). NATS no se tocó: el
    `last_seq` de `storage.turns` siguió en 974 y 310.
  - **Health:** `failed=0 denials=0 srcdrops=0 streamlost=0` en los 4.
  - **Limpieza:**
    - los dos nodos purgados con su identidad (identity volvió a 28 ILKs);
    - el tenant borrado y purgado;
    - las regiones de memoria vaciadas (cognition detenida con `mask --runtime`; no quedó ninguna
      máscara);
    - de la base se borraron 18 turnos, sus 18 filas de inbox, 415 sobres de cognition de la
      inbox y todas las filas de cognition (no queda ninguna).
  - **Para probar:**
    - `kill_node` seguido de `remove_node_instance` falla (`NODE_INSTANCE_RUNNING`): la baja
      completa es `kill_node` con `purge_instance: true`, que además libera la identidad;
    - `state_fetches_total` sale en el STATUS de SY.cognition (un mensaje de sistema `STATUS`),
      no en CONFIG_GET.
- **Pendiente para 2b:**
  - la línea "sy.cognition started" dice `ai_secret_source=missing`, porque la clave ahora se
    resuelve después, en la tarea de turnos. Es cosmético;
  - el ejemplo de `config-set` del admin todavía nombra `vault_key`, rechazado desde 0.1.53. Va
    a la pasada de limpieza.
- **Rollback:** snapshot `pre-stream2a-0-1-77`. `apt install fluxbee=0.1.76` sola no alcanza:
  - las tablas de cognition quedaron en v2, y el SY.storage de 0.1.76 no puede escribirlas;
  - SY.policy salió de la sección propia de cada `hive.yaml`.

## 0.1.76 — El stream del router lleva los turnos de todos los hives a SY.storage (A-70, etapa 1b)

- **Fecha:** 2026-10-16 (ART) · **Versión anterior:** 0.1.75 · **Commit:** `e24e7d3`.
  - Diseño: `docs/router-stream-v1.md`. Una revisión del código: ningún HIGH, los dos MEDIUM y los
    LOW arreglados antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **Router:** cada router tiene su stream, un nodo propio `RT.<router>.stream@<hive>`:
    - copia cada turno y toma los `STREAM_APPEND` de sus nodos;
    - los guarda en disco una ventana de 120 s y 32 MB, y los entrega en lotes con ack a
      `SY.storage@motherbee`;
    - el router nunca espera: una cola acotada por cantidad y por bytes, y el disco en su propio
      hilo;
    - lo que se pierde (vencido, demasiado grande, cola o tope llenos) se cuenta una vez y sale en
      una línea de alarma cada 30 s;
    - un registro queda en disco solo hasta que lo tomaron;
    - los turnos van a NATS solo en el motherbee y los workers, donde todavía los leen cognition y
      policy (etapa 2);
    - el broker borra el consumidor viejo `durable.sy-storage.turns` al arrancar y lo rechaza
      después.
  - **SY.storage:**
    - toma los lotes del stream con su propia conexión: una transacción a la inbox, el ack y
      después el procesamiento, en otra tarea; ya no lee turnos de NATS;
    - cada registro tiene que venir de un nodo del hive del stream, y un turno tiene que nombrar a
      su productor como remitente;
    - `turns` es el log de mensajes: guarda el mensaje entero, `thread_id`, `trace_id`, `ts` (la
      hora del router) y `stored_at`;
    - un backfill único copia los mensajes de la inbox a `turns` (marca en `storage_migrations`);
    - después, la inbox borra lo procesado de más de 24 h.
  - **Archi:** la página Messages lee `turns`. La lista va por `ts`, el tail en vivo por
    `stored_at`, y el detalle muestra el mensaje entero.
  - **SY.admin:** `list_streams`, `GET /hives/{hive}/streams`.
  - **`ops.py health`:** suma `streamlost`, lo que las alarmas de los streams dicen perdido.
- **Deploy:**
  - build de `e24e7d3` en 23 min, publish en 12 s, deploy en 224 s;
  - snapshots `pre-stream1b-0-1-76` en las 4 VMs (se borró `pre-cognition-0-1-73`);
  - 0.1.76 en los 4, health `failed=0 streamlost=0`.
- **Gate (con un nodo de prueba por socket, `AI.gate76probe`, que se manda turnos a sí mismo sin
  `thread_id`, así cognition no gasta):**
  - **La migración:**
    - el backfill completó 5 filas de 0.1.75, re-enlazó las 542 `legacy:` e insertó 35 respuestas
      que `turns` había descartado; 0 ilegibles;
    - `turns` quedó con 582 filas, todas con su mensaje;
    - la inbox borró 435 filas procesadas de más de 24 h;
    - el broker del motherbee ya no tiene `durable.sy-storage.turns`, y su `storage.turns` quedó
      en 0 mensajes.
  - **Los cuatro hives llegan a la base,** 20 de 20 cada uno, con el remitente real y una demora
    de 0,2 s. Antes solo llegaba lo del motherbee.
  - **Archi** lista, abre el detalle (también de la fila más vieja del backfill) y el tail en vivo
    mostró tres turnos de worker1 a 0,15 s.
  - **SY.storage ausente 71 s** mientras worker1 y el ingress mandaban 40 turnos cada uno:
    - el backlog se vio (31 registros en cada stream);
    - al volver se entregó todo, 40 de 40, una sola vez, con la hora original.
  - **SY.storage ausente más de 3 min:**
    - los 10 turnos de worker1 vencieron, contados una vez (`expired=10`), con la línea de alarma;
    - el ruteo siguió;
    - en la base no hay nada de ellos.
  - **El router de worker1 reiniciado con 10 pendientes:** los recuperó del disco y los entregó
    cuando volvió SY.storage.
  - **El router y SY.storage del motherbee reiniciados** mientras worker1 mandaba 120 turnos en
    60 s: llegaron los 120, una vez.
  - **Health:**
    - `streamlost=10` solo en worker1, en la ventana del vencimiento inducido;
    - limpio después.
  - **Para la prueba:** el watchdog de SY.orchestrator levanta un `sy-storage` detenido en 2 s, así
    que la ausencia se hizo con `systemctl mask --runtime` y `unmask`. No quedó ninguna máscara.
  - **Limpieza:** se borraron de `turns` y de la inbox las 373 filas de prueba. No quedan nodos de
    prueba.
- **Rollback:** snapshot `pre-stream1b-0-1-76`. `apt install fluxbee=0.1.75` anda, pero no
  devuelve todo:
  - las columnas nuevas de `turns` no molestan, y el SY.storage viejo vuelve a crear su
    consumidor de NATS;
  - los turnos de los spokes dejan de llegar a la base;
  - el Archi viejo lee la inbox, que ya no tiene lo procesado de más de 24 h. La historia
    completa vuelve solo con el snapshot.

## 0.1.75 — Los bugs vivos bajo el plano del log (A-70, etapa 1a)

- **Fecha:** 2026-10-16 (ART) · **Versión anterior:** 0.1.74 · **Commit:** `1c00389`.
  - Sale de la revisión del diseño del stream del router (`docs/router-stream-v1.md`): bugs que
    ya estaban en PROD, arreglados sobre el NATS de hoy.
  - Una revisión del código, arreglada antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **Router:**
    - un frame de más de 128 KB se salta y se cuenta, y el enlace sigue;
    - un mensaje de otro hive a un nodo de un router que no es el gateway ya no se descarta;
    - `RT.` reservado: un HELLO con ese prefijo solo se acepta del propio proceso del router.
  - **SY.storage:**
    - `turns` usa la clave de la inbox, así que la respuesta de la IA ya no se pierde. La
      migración corre una vez; las filas viejas quedan como `legacy:<ctx>:<seq>`;
    - sale si no puede usar su base, al arrancar o después, y systemd reintenta;
    - reintenta la inbox cada 60 s, en orden y contando intentos;
    - la inbox no se purga (Q7 del doc).
  - **SY.cognition:** confirma cada turno al encolarlo y lo procesa en su propia tarea; si esa
    tarea muere, sale.
  - **SY.orchestrator:** ningún nodo administrado usa `RT.*`, y el prompt de Archi lo dice.
- **Deploy:**
  - antes: ningún hive tenía nodos `RT.*` persistidos;
  - build de `1c00389` en 17 min, publish en 16 s, deploy en 218 s;
  - snapshots `pre-stream1a-0-1-75` en las 4 VMs (se borró `pre-sync-0-1-72`);
  - 0.1.75 con el mismo manifest (`6a07db01`) en los 4, health `failed=0`.
- **Gate:**
  - **La migración de `turns`:** índice único por `dedupe_key`, sin la clave primaria vieja, 542
    filas `legacy:`.
  - **Un pedido y su respuesta con el mismo `trace_id`:** dos filas en `turns`. Se probó con un
    workflow de prueba cuyo mensaje reusa el `trace_id` del evento.
  - **Cognition:** un turno de prueba costó un llamado al etiquetador. En 0.1.74 costaba 3 o 4.
  - **SY.storage:** con su conexión a postgres cortada, salió, y systemd lo levantó con base en 7 s.
  - **`RT.`:** un proceso que no es el router no puede hacer HELLO con un nombre `RT.`. Se rechaza,
    con su PID en el log.
  - **Lo que PROD no puede producir** (un router por hive, el SDK no manda frames de más): el salto
    de frames grandes y la entrega por IRP quedan probados por tests unitarios.
  - **Limpieza:** el tenant de prueba se purgó; PROD quedó como antes.
- **Rollback:** snapshot `pre-stream1a-0-1-75`. `apt install fluxbee=0.1.74` solo no alcanza: la
  migración de `turns` no se deshace, y el código viejo inserta con `ON CONFLICT (ctx, seq)`, que
  ya no tiene índice único, así que los turnos fallarían.

## 0.1.74 — El SDK de IA lee la respuesta de OpenAI de su ítem `message` (A-69)

- **Fecha:** 2026-10-16 (ART) · **Versión anterior:** 0.1.73 · **Commit:** `0d69329`. Sale de lo
  que encontró el gate de 0.1.73.
- **Alcance:** motherbee + los tres spokes (core-update). Los `hive.yaml` siguen en Claude.
- **Qué cambió:** el camino sin herramientas del SDK leía el texto de `output[0]`; con un modelo que
  razona (gpt-5.5) ese es el ítem de razonamiento. Ahora un solo lector para los dos caminos:
  `output_text` si viene, si no el primer ítem `message`.
- **Deploy:** build de `0d69329` en 19 min (el SDK de IA recompila a todos sus consumidores),
  publish en 14 s, deploy en 251 s. Snapshots `pre-sdk-0-1-74` en las 4 VMs (se borró
  `pre-gate-0-1-71`). 0.1.74 con el mismo manifest (`b9d64985`) en los 4, health `failed=0`.
- **Gate:**
  - **Cognition con OpenAI** (solo el del motherbee, un rato): etiquetador y resumidor anduvieron
    (4 y 4 llamados, sin fallas).
  - **De vuelta en Claude:**
    - el etiquetador anduvo;
    - de 3 resúmenes, uno pasó el timeout de 8 s de cognition (`semantic_tagger.timeout_ms`):
      Opus 5.5 es más lento. Ese turno se queda sin síntesis de memoria, sin más daño. Queda para
      el operador: subir ese timeout o usar Sonnet 5.5.
  - **Archi con Claude:** sigue usando sus herramientas (6 s).
  - **Los cuatro consumidores** en Claude, con cognition listo en los dos hives; health `failed=0`.
- **Rollback:** snapshot `pre-sdk-0-1-74`, o `apt install fluxbee=0.1.73`.
- **Después, sobre 0.1.74 (decisión del operador, 2026-10-16):** la infra pasa a
  `claude-sonnet-5-5` (*"vamos por Sonnet 5.5"*).
  - Se cambió el modelo en los dos `hive.yaml` y se reiniciaron los mismos servicios.
  - Archi usa sus herramientas en 3 s (con Opus, 4 a 6 s).
  - Cognition: 3 de 3 etiquetados y 3 de 3 resúmenes, sin timeouts.

## 0.1.73 — La infra de IA pasa a Claude; SY.cognition deja de mandar `temperature` (A-69)

- **Fecha:** 2026-10-16 (ART) · **Versión anterior:** 0.1.72 · **Commit:** `822f237`. Pedido del
  operador: probar su key de Anthropic en toda la infra y, si no anda, volver a OpenAI.
- **Alcance:** motherbee + los tres spokes (core-update). Además, sobre 0.1.72 y antes del build,
  el `hive.yaml` del motherbee y de worker1 (los únicos con sección `ai`; ingress y egress no corren
  IA):
  - `default_provider: anthropic`, con `claude-opus-5-5`;
  - la entrada de OpenAI (`gpt-5.5`) sigue declarada: volver es cambiar una línea y reiniciar
    `sy-admin`, `sy-architect`, `sy-cognition` y `sy-frontdesk-gov` (en worker1, `sy-cognition`);
  - backup `hive.yaml.pre-claude-0-1-72` en los dos.
- **La prueba sobre 0.1.72, consumidor por consumidor:**
  - **Archi:** contesta con Claude (2 s) y usa sus herramientas: listó los nodos de worker1 con
    `fluxbee_system_get` (4 s).
  - **El admin y SY.frontdesk.gov:** resuelven la key de Anthropic (`CONFIGURED`). No mandan
    `temperature`.
  - **SY.cognition:** con Opus 5.5 y con Sonnet 5.5, su etiquetador falló en un turno de prueba:
    "`temperature` is deprecated for this model". De vuelta en OpenAI falló igual ("not
    supported"): cognition estaba roto con los dos proveedores (A-69). Se volvió a Claude, que anda
    en todo lo que anda OpenAI, y se arregló cognition.
- **Qué cambió (`822f237`):** cognition sin `temperature`, topes de salida de 1024 tokens, y la key
  de IA resuelta al arrancar (antes decía `degraded_no_ai_provider` hasta su primer turno).
- **Deploy:** build de `822f237` en 7 min, publish en 12 s, deploy en 229 s. Snapshots
  `pre-cognition-0-1-73` en las 4 VMs (se borró `pre-wf-0-1-70`). 0.1.73 con el mismo manifest
  (`13af35fc`) en los 4, health `failed=0`.
- **Gate:**
  - **Cognition en los dos hives:** `ready_semantic_ai` con Claude apenas arrancó, también en
    worker1 recién actualizado.
  - **Un turno de prueba en el motherbee:**
    - el etiquetador devolvió tags reales (`billing`, `duplicate charge`, `october invoice`…);
    - el resumidor narrativo corrió con `claude-opus-5-5`;
    - ninguna falla.
  - **Encontrado, arreglado para 0.1.74:** con OpenAI el etiquetador ya no falla por
    `temperature`, pero el SDK no encuentra el texto de la respuesta ("missing text output"). Un
    modelo que razona pone primero un ítem de razonamiento, y el SDK leía el primer ítem.
  - **Datos de prueba:** los turnos de prueba quedaron en el log de mensajes y en la memoria de
    cognition, con hilos `claude-probe-20261016*` y `openai-probe-20261016*`.
- **Rollback:** snapshot `pre-cognition-0-1-73`, o `apt install fluxbee=0.1.72`. Para volver a
  OpenAI, `default_provider: openai` en los dos `hive.yaml` y reiniciar esos servicios.

## 0.1.72 — Un deploy de WF en un worker hace que el motherbee anuncie su paquete (A-68, gate de 0.1.71)

- **Fecha:** 2026-10-16 (ART) · **Versión anterior:** 0.1.71 · **Commits:** `a053102` y `17e1edd`.
  Sale de lo que encontró el gate de 0.1.71.
  - `a053102` pedía escanear también en el worker y esperar a que su carpeta quedara quieta.
  - Leído contra Syncthing 2.0.14 antes de publicarlo, no servía: el worker trae apenas el
    motherbee anuncia, y un escaneo en el worker no trae nada.
  - Se corrigió en `17e1edd`. El paquete armado con `a053102` no se publicó.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **En un worker, después de publicar,** SY.wf-rules le pide al admin un `sync_hint` sobre el
    `fluxbee-dist-runtimes` del motherbee. El motherbee escanea ya y anuncia el paquete.
  - **Mientras el paquete no esté,** pide el mismo escaneo antes de cada reintento.
- **Deploy:** build de `17e1edd` en 6 min, publish en 9 s, deploy en 176 s. Snapshots
  `pre-sync-0-1-72` en las 4 VMs (se borró `pre-teardown-0-1-69`). 0.1.72 con el mismo manifest
  (`acbb2efd`) en los 4, health `failed=0`.
- **Gate, en el peor caso:** recién hecho el core-update, y el E2E de worker1 justo después del
  borrado del E2E del motherbee (lo que falló en 0.1.71).
  - **El E2E del ciclo de vida en el motherbee:** 35/35.
  - **El E2E en worker1:** 35/35.
    - El primer deploy tardó 4 s: el paquete llegó 1 s después de publicado, en el mismo pull que
      el borrado del workflow anterior.
    - La v2 llegó en 2 s.
    - El borrado después del rollback no dejó paquetes (el arreglo de 0.1.71).
  - **Por Archi:**
    - el deploy y el borrado tardan 1 s cada uno;
    - el nodo arranca `HEALTHY` con `launched_by: archi`;
    - una clave de `_system` que no conoce queda en el archivo, y sigue `HEALTHY`.
  - **Un anuncio le gana a la espera de Syncthing:** un pull del manifest falló y Syncthing anotó
    que reintentaba en 1 min; con el anuncio siguiente lo trajo al segundo.
  - **Limpieza:** el tenant de prueba (`wf-gate-0170`) se borró y se purgó. PROD quedó como antes:
    solo el tenant raíz, los 28 ILK del core, ningún nodo WF y solo `wf.engine` entre los
    runtimes `wf.*`.
- **Rollback:** snapshot `pre-sync-0-1-72`, o `apt install fluxbee=0.1.71`.

## 0.1.71 — Un deploy de WF en un worker espera su paquete; el borrado deja la versión vigente para el final (A-68, gate de 0.1.70)

- **Fecha:** 2026-10-16 (ART) · **Versión anterior:** 0.1.70 · **Commit:** `73f7132`. Sale de lo
  que encontró el gate de 0.1.70.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **SY.wf-rules** espera el paquete de un workflow hasta 45 s, con un plazo de 60 s por pedido;
    el admin lo espera 75 s y Archi 90 s.
  - **Al borrar un workflow,** una versión que el admin rechaza como vigente
    (`RUNTIME_CURRENT_CONFLICT`) se reintenta cuando las demás ya no están.
- **Deploy:** build de `73f7132` en 12 min, publish en 14 s, deploy en 186 s. Snapshots
  `pre-gate-0-1-71` en las 4 VMs (se borró `pre-cloud-0-1-68`). 0.1.71 con el mismo manifest
  (`94ae3776`) en los 4, health `failed=0`.
- **Gate:**
  - **El E2E del ciclo de vida en el motherbee:** 35/35.
  - **Encontrado en worker1, arreglado en 0.1.72:** el primer deploy esperó 45 s y el paquete no
    llegó (`RUNTIME_NOT_PRESENT`).
    - El manifest llegó a los 12 s y los archivos del paquete a los 62 s.
    - El watcher del motherbee agrupa los cambios mientras la carpeta de runtimes está agitada, y
      la del worker espera para reintentar después de fallar al borrar directorios.
    - La limpieza del E2E borró el workflow y no dejó paquetes.
- **Rollback:** snapshot `pre-gate-0-1-71`, o `apt install fluxbee=0.1.70`.

## 0.1.70 — Los nodos WF por el camino estándar: SY.wf-rules por el admin, el plano de control en el SDK de Go (A-68)

- **Fecha:** 2026-10-16 (ART) · **Versión anterior:** 0.1.69 · **Commit:** `c289bb3`. Diseño en
  `docs/wf-standard-path-v1.md`, aprobado por el operador el 2026-10-15. Una revisión (1
  bloqueante, 7 a arreglar), arreglada antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **SY.admin** atiende cada comando de la malla en su propia tarea, y un lock serializa los
    cambios del manifest de runtimes. El deploy de un workflow desde Archi vuelve a andar.
  - **SY.wf-rules** lanza, reinicia, configura y borra sus nodos WF por el admin, que lo deja
    actuar solo sobre los `WF.*` de su hive. Acepta operaciones solo del admin.
  - **`launched_by`** es quien pidió el deploy: `operator` por HTTP, `archi` desde Archi.
  - **La política de sistema:** la regla (4) queda solo para SY.admin, sin SY.wf-rules ni
    `WF.orch.diag`.
  - **El SDK de Go** tiene el plano de control de los nodos administrados y el nodo WF lo usa: con
    una config que no puede usar queda arriba, en FAILED_CONFIG y con el motivo.
  - **El orquestador** no arranca un paquete de workflow sin su `flow/definition.json`
    (`RUNTIME_NOT_PRESENT`).
- **Deploy:** build de `c289bb3` en 25 min, publish en 14 s, deploy en 221 s. Snapshots
  `pre-wf-0-1-70` en las 4 VMs (se borró `pre-tenant-0-1-67`). 0.1.70 con el mismo manifest
  (`e3986977`) en los 4, health `failed=0`.
- **Gate, con un tenant de prueba, por el HTTP del operador y por Archi:**
  - **El E2E del ciclo de vida en el motherbee** (`scripts/wf_rules_lifecycle_e2e.sh`): 35/35.
    - Compile, apply con spawn, estado, update, rollback y delete, todo por el admin.
    - El nodo queda con `launched_by: operator`.
    - Con un error en su config queda `RUNNING` y `DEGRADED` con `invalid_config`, sin
      reinicios; con la config arreglada y un reinicio vuelve a `HEALTHY`.
  - **Por Archi** (SCMD por la malla):
    - el deploy tarda 1,3 s y el nodo arranca `HEALTHY` con `launched_by: archi`;
    - una clave de `_system` que el nodo no conoce queda en el archivo y el nodo sigue `HEALTHY`;
    - el borrado tarda 1,3 s y el nodo desaparece.
  - **Encontrado en worker1, arreglado en 0.1.71:**
    - la actualización no esperó lo suficiente al paquete nuevo (`restart_failed`);
    - el borrado dejó los paquetes en el motherbee (`RUNTIME_CURRENT_CONFLICT`). Se borraron por
      el admin.
- **Rollback:** snapshot `pre-wf-0-1-70`, o `apt install fluxbee=0.1.69`. En PROD no hay nodos WF.

## 0.1.69 — Cloud borra sus nodos sin trabar al admin; un SY de otro hive reintenta mientras identity arranca (A-67)

- **Fecha:** 2026-10-15 (ART) · **Versión anterior:** 0.1.68 · **Commit:** `6f03310`. Sale de lo
  que encontró el gate de 0.1.68.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **SY.admin:** los comandos que llegan por la malla y hacen que el orquestador lo vuelva a
    llamar (kill y remove de nodos con purga) corren en su propia tarea. Antes el `delete_node` de
    Cloud se trababa: el admin esperaba al orquestador y el orquestador al admin.
  - **IO.cloud:** espera 25 s para `stop_node` y `delete_node`.
  - **SY.vault:** a un SY de otro hive que la SHM todavía no tiene le contesta
    `IDENTITY_UNAVAILABLE`, que se reintenta, en vez de `UNAUTHORIZED`. No entrega nada de más.
- **Deploy:** build de `6f03310` en 9 min, publish en 16 s, deploy en 237 s. Snapshots
  `pre-teardown-0-1-69` en las 4 VMs (se borró `pre-secrets-0-1-66`). 0.1.69 con el mismo manifest
  (`baf673f4`) en los 4, health `failed=0`.
- **Gate:**
  - **El reinicio:** `SY.cognition@worker1` volvió a leer una clave del génesis en el hueco en que
    identity arrancaba, y esta vez recibió `IDENTITY_UNAVAILABLE`; después leyó bien. Ningún
    `UNAUTHORIZED` de un SY.
  - **`delete_node` por la puerta pública:** contesta ok; la clave que el nodo tenía como propia se
    borra y la asignada queda; el nombre queda libre (se volvió a crear y a borrar).
  - **Limpieza:** PROD quedó como antes (solo el tenant raíz y los 28 ILK del core).
- **Rollback:** snapshot `pre-teardown-0-1-69`, o `apt install fluxbee=0.1.68`.

## 0.1.68 — Cloud maneja sus tenants y los nodos IO que lanzó; parar un nodo dura (A-67, etapa 4)

- **Fecha:** 2026-10-15 (ART) · **Versión anterior:** 0.1.67 · **Commits:** `a5f1b2a` (el código del
  rechazo de `set_ilk_tenant`), `b26de82` (etapa 4) y `1b47c3e` (la revisión). Diseño en
  `docs/ilk-tenant-model-v1.md` §2.4 y D14, decidido con el operador el 2026-10-15. Una revisión (3
  mayores, 4 menores), arreglada antes del build; el build que traía el primer mayor se frenó.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **Tenants por Cloud:** `update_tenant` con estado (`active`, `suspended`), settings y sponsor;
    `get_tenant` completo por el admin; `list_tenants` sin el génesis.
  - **Nodos por Cloud:** `list_nodes`, `get_node_status`, y `start_node`, `stop_node` y
    `delete_node` solo para los nodos IO que lanzó en su tenant (el admin anota quién lanzó cada
    nodo; el hive del nodo, o el motherbee si el hive es de una versión anterior, chequea la
    condición).
  - **Parar dura, para todos.**
  - **El nodo WF en Go** conoce `launched_by`.
- **Deploy:** build de `1b47c3e` en 8 min, publish en 15 s, deploy en 216 s. Snapshots
  `pre-cloud-0-1-68` (se borró `pre-scope-0-1-65`). 0.1.68 en los 4 (`a41a6af7`), health
  `failed=0`, OPA `in_sync` 4/4.
- **Gate, por la puerta pública con el bearer de Cloud y del lado del operador:**
  - **El catálogo** lista 24 operaciones, con las 7 nuevas.
  - **Tenants:**
    - settings y sponsor editados y leídos de vuelta con `get_tenant`;
    - `pending` rechazado (`INVALID_REQUEST`) y el génesis rechazado (`TENANT_ROOT_PROTECTED`);
    - `list_tenants` sin el génesis.
  - **Nodos:**
    - s1, lanzado por Cloud y configurado al lanzarlo, aparece en `list_nodes` con
      `launched_by: cloud` y solo su estado;
    - un nodo de otro tenant da `NODE_NOT_FOUND`;
    - o1, lanzado por el operador en el mismo tenant, le da `NODE_NOT_FOUND` a Cloud para arrancar,
      parar y borrar;
    - un nodo que no es IO da `INVALID_REQUEST`.
  - **Parar dura:** Cloud paró s1 y siguió abajo más de una pasada del relanzamiento; arrancado,
    volvió con la marca de relanzar.
  - **Suspender por Cloud:** un nodo nuevo, `TENANT_NOT_ACTIVE`; el vault le niega la clave a s1;
    reactivado, la vuelve a leer.
  - **`a5f1b2a`:** mover un temporal de un tenant que no está borrado contesta
    `INVALID_TENANT_TRANSITION` solo, con el detalle aparte.
  - **Un workflow de prueba:** su nodo WF arrancó limpio (0 reinicios, `launched_by: system`).
  - **Encontrado, arreglado en 0.1.69:** `delete_node` desde Cloud se trababa (timeout, la
    identidad marcada sin purgar), y un SY de otro hive recibió un `UNAUTHORIZED` transitorio
    mientras identity arrancaba.
  - **Limpieza:** la identidad trabada purgada, o1 matado, personas, claves y tenants borrados por
    Cloud y purgados. PROD quedó como antes.
- **Rollback:** snapshot `pre-cloud-0-1-68`, o `apt install fluxbee=0.1.67`. Un nodo parado con 0.1.68
  queda con `relaunch_on_boot: false`; con 0.1.67 tampoco se relanza solo.

## 0.1.67 — Un tenant que no está activo no arranca nodos y borrarlo no marca a nadie (A-66, etapa 3); el vault ata el ILK al nodo que manda (A-65)

- **Fecha:** 2026-10-15 (ART) · **Versión anterior:** 0.1.66 · **Commits:** `fbca8e7` y `81d2fab`
  (A-65), `11aead9` (etapa 3) y `e85041d` (la revisión). Diseño en `docs/ilk-tenant-model-v1.md`
  §2.3, decidido con el operador. Una revisión de A-65 (alcance parcial, comparación exacta) y una
  de la etapa 3 (2 mayores, 2 menores, 4 detalles), arregladas antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **SY.vault (A-65):** el ILK de quien pide tiene que ser el del nodo que el router dice que
    mandó: derivado del nombre para un SY, el registrado para su nombre para cualquier otro. Entre
    hives está cerrado; en el mismo hive el nombre sigue siendo el del HELLO (P-1).
  - **Orquestador:** `run_node`, start y restart contestan `TENANT_NOT_ACTIVE` para un tenant
    pendiente o suspendido, y los relanzamientos lo saltean. Un nodo que ya corre no se mata.
  - **SY.identity:**
    - borrar un tenant marca solo al tenant;
    - purgarlo se rechaza con personas cargadas (`TENANT_HAS_PEOPLE`);
    - `set_ilk_tenant` mueve temporales desde un tenant borrado;
    - `restore_ilk` devuelve a una persona con su tenant borrado;
    - la migración v4 desmarca a quien marcó una cascada vieja.
  - **io.api `by_ilk`:** personas cargadas al tenant o con un canal en su espacio (D12).
  - **Admin y Cloud:** los códigos nuevos son 409; textos y catálogo.
- **Deploy:**
  - **Build:** `e85041d` en 20,5 min. Antes se lanzaron por error dos builds del mismo commit a la
    vez (`11aead9`); se pararon a los segundos y quedó uno solo.
  - **Publish y deploy:** publish en 19 s, deploy en 293 s.
  - **Snapshots:** `pre-tenant-0-1-67` en las 4 VMs (se borró `pre-cloud-0-1-64`).
  - **Estado:** 0.1.67 con el mismo manifest (`637ed35d`) en los 4, health `failed=0`, OPA `in_sync`
    4/4. Identity: *"migrations through v4"*, sin nada que desmarcar (PROD no tenía marcas de
    cascada).
  - **Antes del deploy:** el SQL de la v4 se probó sobre el esquema de PROD dentro de una
    transacción que se deshizo: desmarcó al libre y a uno de dos con el mismo email; dejó marcados al
    del email tomado, al otro y a un nodo; no tocó una marca directa.
- **Gate:**
  - **A-65:**
    - después del reinicio, todos los SY leyeron sus claves sin un rechazo, también los de otros
      hives (`SY.cognition@worker1`, `SY.edge@ingress1`);
    - `send_node_message` del admin al vault con el ILK de IO.cloud: `UNAUTHORIZED`, auditado; con
      el suyo: pasa;
    - el nodo IO.slack del gate lee su clave con su propio ILK.
  - **Etapa 3, por la puerta pública con el bearer de Cloud y del lado del operador:**
    - **Armado:** tenants T y U; un nodo IO.slack s1 en T con la clave de T general; Ana y Bob
      (cuentas) y un alta sin nombre, que queda pendiente (un temporal); Bob borrado.
    - **T suspendida:**
      - el restart de s1 da `409 TENANT_NOT_ACTIVE` y s1 sigue corriendo;
      - un nodo nuevo en T da `TENANT_NOT_ACTIVE`;
      - el vault le niega la clave a s1 (7 `UNAUTHORIZED`);
      - con la unidad de s1 parada, dos pasadas seguidas lo saltean (*"not relaunched: its tenant
        is not active"*);
      - mover el temporal fuera de T, que no está borrada, se rechaza.
    - **T reactivada:** la pasada siguiente (52 s después) relanzó s1, que volvió a leer.
    - **Borrar:**
      - con s1 vivo, `TENANT_HAS_NODES`;
      - con s1 purgado, ok y `people_charged: 2`;
      - Ana y el temporal no se marcan y siguen cargados a T, y la cuenta de Ana contesta por email;
      - Bob vuelve con T borrada;
      - restaurar T da `people_charged: 3`.
    - **Purgar:** `TENANT_HAS_SECRETS`; sin la clave, `TENANT_HAS_PEOPLE` con los tres; los tres a U
      (el temporal desde T borrada); purga ok sin llevarse a nadie; los tres siguen en U y el
      temporal sigue pendiente.
    - **Limpieza:** las tres personas borradas por Cloud, U borrada y purgada (se fueron las tres
      marcadas). PROD quedó como antes: solo el tenant raíz y los 28 ILK del core.
  - **Encontrado en el gate:** el rechazo de mover un temporal contestaba
    `INVALID_TENANT_TRANSITION|<detalle>` como `error_code`. Arreglado en `a5f1b2a`; sale con el
    próximo release.
  - **Lo que no se probó en vivo:** io.api `by_ilk` (D12), cubierto por tests; la v4 sobre datos
    reales, porque PROD no tenía (cubierta por la prueba en transacción).
- **Rollback:**
  - snapshot `pre-tenant-0-1-67`;
  - o `apt install fluxbee=0.1.66`. La v4 no se deshace: a quien desmarcó, 0.1.66 lo ve activo, y
    un tenant borrado sin cascada queda con su gente activa (0.1.66 no la marca de vuelta).

## 0.1.66 — Un secreto lo usa su dueño, sus nodos asignados o, si es general, los nodos de su tenant; en el génesis nace solo el core (A-64, etapa 2)

- **Fecha:** 2026-10-14 (ART) · **Versión anterior:** 0.1.65 · **Commits:** `80b479c` (altas
  pendientes, D11) y `96c8d77` (etapa 2). Diseño en `docs/ilk-tenant-model-v1.md` §2.2, decidido con
  el operador. Una revisión del código (1 mayor, 9 menores), arreglada antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **SY.vault:**
    - nodos asignados en el secreto y `VAULT_ASSIGN`, que los cambia sin tocar el valor;
    - el valor sale solo con el tenant del secreto activo;
    - un valor nuevo conserva quién lo usa;
    - el aviso de cambio llega a quien lo tenía y a quien lo tiene.
  - **SDK:** la resolución busca primero uno propio, después uno asignado, después el general del
    tenant; nunca el del raíz.
  - **SY.admin:** `vault_assign` y nodos asignados en `vault_put`, siempre del tenant del secreto y
    nunca personas.
  - **IO.cloud:** `nodes` en `put_token`, la operación `assign_token` y `pending_signup` en las
    lecturas.
  - **Orquestador:** en el génesis nace solo el core.
  - **SY.identity:** el tenant de un nodo no cambia, y las altas pendientes se leen como tales.
- **Deploy:**
  - **Primer build:** murió por memoria. La VM de build (4 CPU, 8 GB, sin swap) mató un `rustc`;
    0.1.64 y 0.1.65 habían pasado justo. Se le sumó un swapfile de 8 GB (persistente).
  - **Segundo build:** `96c8d77` en 18 min.
  - **Publish y deploy:** publish en 13 s, deploy en 216 s.
  - **Snapshots:** `pre-secrets-0-1-66` en las 4 VMs (se borró `pre-a58-0-1-63`).
  - **Estado:** 0.1.66 con el mismo manifest (`648d8493`) en los 4, health `failed=0`, OPA `in_sync`
    4/4.
  - **Antes del deploy:** se verificó que en el génesis de PROD corre solo el core, así que la regla
    nueva no dejaba nada afuera.
- **Gate (medido con la auditoría del vault, que registra cada lectura de cada nodo):**
  - **Después del reinicio:** el core leyó todas sus claves (SY.\* las del génesis, SY.edge las
    suyas), sin un rechazo.
  - **Por la puerta pública, con el bearer de Cloud: 22 controles, todos PASS.**
    - Dos empresas A y B. Cuatro nodos IO.slack: tres en A y uno en B, todos configurados con la
      clave de A, con tokens falsos.
    - Una persona en A.
    - `put_token` asignado a dos nodos; `list_tokens` los muestra.
    - Asignar un nodo de B, o una persona, da `INVALID_REQUEST`; asignar desde B la clave de A da
      `KEY_NOT_FOUND`.
    - Las lecturas de identidades traen `pending_signup`.
  - **Lo que leyó cada nodo:**
    - **Asignada a g1 y g2:** leen g1 y g2; g3 (de A, no asignado) y b1 (de B) reciben
      `UNAUTHORIZED`.
    - **Asignada solo a g1:** g2 quedó afuera apenas llegó el aviso y soltó la credencial.
    - **General para A:** leen g1, g2 y g3; b1 no.
    - **A suspendida:** los tres de A, rechazados con *"the secret's tenant … is not active
      (suspended)"*. **Reactivada:** vuelven a leer.
    - **El secreto de una persona** (dueña Juan, asignado a g4): g4 lo lee, y lo sigue leyendo
      después de cambiar a Juan a la empresa B.
  - **Del lado del operador:** un nodo AI en el génesis da `TENANT_ROOT_NOT_ALLOWED`, y
    `?pending_signup=true` contesta (0 pendientes).
  - **Lo que no se probó en vivo:** el cambio de tenant de un nodo, que no tiene camino de API para
    intentarlo. Está cubierto por tests.
- **Limpieza:** los cinco nodos purgados (al purgarlos no se borró ningún secreto, porque no eran
  dueños de ninguno), las claves y la persona borradas por Cloud, los dos tenants purgados. Identity
  y el vault quedaron como antes.
- **Arreglado después, sale con el próximo release:** el admin HTTP devolvía 500 para
  `TENANT_ROOT_NOT_ALLOWED`; ahora 403.
- **Rollback:**
  - snapshot `pre-secrets-0-1-66`;
  - o `apt install fluxbee=0.1.65`. No hay migración: los secretos que 0.1.66 escribe con nodos
    asignados, 0.1.65 los ve generales.

## 0.1.65 — Un ILK por actor: el ámbito de cada canal, una cuenta de Cloud por email y `set_ilk_tenant` (A-63, etapa 1)

- **Fecha:** 2026-10-14 (ART) · **Versión anterior:** 0.1.64 · **Commit:** `8b3e951` (FINDINGS A-63;
  diseño en `docs/ilk-tenant-model-v1.md`, etapa 1). Una revisión del diseño (7 mayores, 5 menores)
  y una del código (1 mayor, 8 menores), todo arreglado antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **SY.identity:**
    - cada canal guarda su ámbito (`scope`) y toda clave sale de él: memoria, base, SHM y el camino
      rápido. Reasignar un ILK no mueve ninguna clave;
    - una cuenta de Cloud por email, en el ámbito de la instalación, que solo escribe IO.cloud (por
      provision, add, update, fusión y push de réplica);
    - `ILK_SET_TENANT`, con historial en la misma transacción (`identity_ilk_tenant_history`);
    - el registro ya no cambia el tenant de una persona, y las fusiones quedan dentro de un tenant;
    - borrar un tenant con cuentas cargadas da `TENANT_HAS_ACCOUNTS`, y un alta sin terminar ni
      traba ni se marca (`pending_signups`);
    - purgar un tenant se lleva los canales que otros ILKs tenían en su ámbito
      (`dropped_channels`);
    - la migración v3 corre en cada arranque del primario, antes de cargar la memoria: mueve las
      cuentas no ambiguas y reporta el resto por log.
  - **SY.admin:** `set_ilk_tenant` (HTTP `PUT .../identity/ilks/{ilk}/tenant` y relay de Cloud),
    con el "para quién" sellado por el admin; Cloud borra y restaura solo personas.
  - **IO.cloud:** `register_human` devuelve la cuenta existente (`existing: true`); `set_ilk_tenant`;
    `get_ilk`, `get_ilk_details`, `delete_ilk` y `restore_ilk` sin tenant.
  - **Contrato:** `docs/io-cloud-api.md` (§9, 0.1.65).
- **Deploy:**
  - build `8b3e951` en 23 min;
  - publish en 15 s;
  - deploy en 230 s;
  - snapshots `pre-scope-0-1-65` en las 4 VMs (se borró `pre-identity-0-1-62`);
  - versiones 0.1.65 con el mismo manifest (`7515c798`) en los 4, health `failed=0`, OPA `in_sync`
    4/4.
- **Antes del deploy, datos viejos a propósito (en 0.1.64, por la puerta de Cloud):**
  - el mismo email registrado desde dos tenants (dos personas);
  - un email único;
  - una persona borrada.
- **Gate:**
  - **La migración, en el arranque:**
    - v3 registrada;
    - el email único pasó al ámbito de la instalación;
    - el email duplicado quedó donde estaba, reportado por log (`ambiguous_emails=1` con los dos
      ILKs);
    - la persona borrada no se tocó.
  - **Por la puerta pública, con el bearer de Cloud: 30 controles, todos PASS.**
    - El email duplicado da `ACCOUNT_AMBIGUOUS`.
    - El único, pedido desde otro tenant, devuelve su cuenta (`existing: true`, sigue en su tenant).
    - Una cuenta nueva desde dos tenants es un solo ILK.
    - `set_ilk_tenant` la mueve (y la segunda vez `changed: false`); el raíz está protegido.
    - El restore de la persona borrada antes del upgrade la deja en el ámbito de la instalación,
      donde `register_human` la encuentra.
    - Lectura por id entre tenants; `delete_ilk` de un nodo da `ILK_TYPE_MISMATCH`; borrar un
      tenant con una cuenta cargada da `TENANT_HAS_ACCOUNTS`.
  - **Del lado del operador:**
    - la fila de historial dice `SY.admin@motherbee for IO.cloud@motherbee`;
    - la SHM de la réplica en worker1 muestra el tenant nuevo y el canal en el ámbito de la
      instalación;
    - purgar el tenant del que se fue una persona funcionó (`dropped_channels=1`; era el MAJOR de
      la revisión del código);
    - reiniciar sy-identity: las claves sobrevivieron la recarga de la base, el reporte quedó
      limpio y el email que había quedado único pasó al ámbito de la instalación;
    - health `failed=0` en los 4 después del reinicio.
- **Limpieza:** las cinco personas borradas por Cloud, los tres tenants purgados por el operador,
  el nodo de prueba purgado. Identity quedó como antes (solo el tenant raíz, sin personas, sin
  historial) y el vault en sus claves de infraestructura más `anthropic_root_pool`.
- **Arreglado después, sale con el próximo release:**
  - el admin HTTP mapeaba `TENANT_NOT_FOUND` a 500: ahora 404, y los rechazos del ciclo de vida de
    identity (`TENANT_HAS_NODES`, `TENANT_NOT_DELETED`, `ILK_KEY_CONFLICT`, …) dan 409;
  - el log del esquema decía "baseline v1" con la v3 aplicada.
- **Rollback:**
  - snapshot `pre-scope-0-1-65`;
  - o `apt install fluxbee=0.1.64`. La migración v3 no se deshace, pero 0.1.64 vuelve a calcular
    las claves desde el tenant del ILK y reescribe el ámbito de cada canal en su próxima escritura.
    La tabla de historial queda, sin uso.

## 0.1.64 — Cloud gestiona las credenciales y el nombre de su empresa; toda escritura al vault nombra su tenant (A-61)

- **Fecha:** 2026-10-13 (ART) · **Versión anterior:** 0.1.63 · **Commit:** `f8c73c5` (FINDINGS
  A-61, A-62). Una sola revisión adversarial: un MAJOR (el factory reset) y seis menores, todos
  arreglados antes del build.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **SY.vault, para todos los llamadores:**
    - un put de una clave de otro tenant es `KEY_TAKEN` y nunca la mueve;
    - borrar, rotar y volver atrás llevan `tenant_id`, y una clave de otro tenant contesta
      `KEY_NOT_FOUND`, con el mismo texto que una inexistente;
    - el admin pone el raíz si no viene, acepta `?tenant_id=` por HTTP y tiene el campo en los planes
      de Archi;
    - el orquestador y el factory reset borran cada clave en su tenant.
  - **SY.identity:**
    - ILK_GET con tenant;
    - `update_tenant` rechaza el raíz y un nombre o dominio de otro tenant, y valida todo antes de
      cambiar nada.
  - **IO.cloud:**
    - la regla 3: el tenant va solo en la raíz del sobre, nunca es el raíz, y todo es de ese tenant;
    - `list_tokens`, `delete_token` y `update_tenant`;
    - todo error con `error_code`.

    SY.admin lo vuelve a exigir del lado del servidor. Archi y la limpieza de un nodo conservan sus
    acciones por la malla.
  - **Contrato:** `docs/io-cloud-api.md` reescrito.
- **Deploy:**
  - build `f8c73c5` en 28,5 min (tocó el SDK);
  - publish en 12 s;
  - deploy en 233 s;
  - snapshots `pre-cloud-0-1-64` en las 4 VMs (se borró `pre-ssh-0-1-61`);
  - versiones 0.1.64 con el mismo manifest en los 4, health `failed=0`, OPA `in_sync` 4/4.
- **Antes del deploy:** en 30 días PROD no tuvo tráfico real de Cloud, solo los e2e del 2026-09-28
  y sondas. El cambio de contrato (el tenant de las lecturas pasa a la raíz del sobre) no rompió a
  nadie en PROD. El hive de DEV del equipo de Cloud tiene que coordinarse antes de pasar a 0.1.64.
- **Gate:**
  - **Por la puerta pública real, desde afuera, con el bearer de Cloud:** 53 controles, todos PASS.
    - El catálogo ofrece 16 operaciones.
    - Regla 3: el tenant en `params`, el raíz y una grafía en mayúsculas son rechazados; sin tenant
      hay error con código.
    - Credenciales: put de A; la misma clave desde B es `KEY_TAKEN`, también con el mismo valor.
    - Claves reservadas: rechazadas al escribir y al borrar.
    - `list_tokens`: A ve su clave con la metadata y sin el valor, y B no ve nada.
    - Borrar desde B la clave de A da `KEY_NOT_FOUND`, con el mismo texto que una inexistente; el
      borrado propio funciona.
    - `update_tenant`: el renombre funciona; el nombre y el dominio de B están tomados; el status es
      del operador; vaciar el dominio funciona; el raíz está protegido.
    - Una persona en A: B no la ve ni por id ni por email; el primer login por email la encuentra en
      A.
    - `delete_ilk` desde B da `ILK_TENANT_MISMATCH`.
    - Nodos: `provision_node` en el raíz está protegido; un nodo en A con un token propio funciona.
  - **Del lado del operador:**
    - matar el nodo con purga borró su secreto del tenant A;
    - `DELETE` por HTTP sin tenant da 404 para una clave de A, y con `?tenant_id=` la borra;
    - la misma clave del raíz desde A da 409 `KEY_TAKEN`;
    - Archi corrió por la malla `vault_list` y `get_ilk`. Antes de esta versión, `get_ilk` le
      quedaba rechazado.
- **Limpieza:** los dos tenants marcados por Cloud y purgados por el operador (con la persona), el
  nodo purgado, las dos sesiones de Archi borradas. El vault quedó en sus 4 claves de
  infraestructura.
- **Visto de paso (menor, sin arreglar):** `GET` de un tenant purgado contesta 500
  `TENANT_NOT_FOUND`; el código no está en el mapa de 404 del admin HTTP.
- **Rollback:**
  - snapshot `pre-cloud-0-1-64`, o `apt install fluxbee=0.1.63`;
  - no hay migración de datos: los registros del vault no cambiaron de forma.

## 0.1.63 — Syncthing corre como `fluxbee` en todos los hosts (A-58); los blobs se escriben y se leen por descriptor y toman el dueño de su carpeta (A-56)

- **Fecha:** 2026-10-06 (ART) · **Versión anterior:** 0.1.62 · **Commits:** `02f7fdb`, `506151c`,
  `aab0e3e` (FINDINGS A-56, A-58, A-60).
- **Revisión:** dos revisiones adversariales (SDK/io.blob y orquestador), dos rondas cada una. Los
  números de los arneses están en A-56.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **Syncthing como `fluxbee`:**
    - el orquestador crea el usuario `fluxbee` en cada host, y Syncthing corre siempre con él;
    - en worker1 y egress1 el cambio es un paso único y visible: para, traspasa sus árboles y
      arranca.
  - **Dueños de `blob/`:**
    - `blob/` es root:fluxbee;
    - staging y `agent-assets` son de root;
    - `active/` y `public/` son de `fluxbee`.
  - **SDK e io.blob:** escriben por descriptor y le entregan cada blob al dueño de su carpeta. Los
    lectores usan `read_blob`, que verifica tamaño y hash. ai.generic verifica el hash de cada asset.
  - **Postinst:** se fue el `chown -R`, que tapaba el problema.
- **Deploy:**
  - build `aab0e3e` en 21 min, publish 10 s, deploy 193 s;
  - snapshots `pre-a58-0-1-63` en las 4 VMs (se borró `pre-hardening-0-1-60`);
  - versiones 0.1.63 en los 4, health `failed=0`, OPA `in_sync` 4/4.
- **Gate A-58:**
  - worker1 y egress1 crearon el usuario `fluxbee`;
  - su Syncthing quedó con unit y proceso `fluxbee` desde las 01:29:27 y 01:29:22, y la alerta
    `syncthing_user_switched` salió recién con el servicio sano;
  - home, `active/` (worker1) y dist pasaron a `fluxbee`;
  - el motherbee y el ingress no se reiniciaron (Syncthing arriba desde antes del deploy);
  - todas las carpetas de los 4 hosts quedaron `idle` y sin errores.
- **Gate A-56:**
  - Motherbee:
    - `blob/` pasó a root:fluxbee 0750;
    - staging (10 entradas) y `agent-assets` (5) pasaron a root;
    - `active/` y `public/` sin entradas ajenas.
  - Ingress: el traspaso del arranque pasó el `.stfolder` de root, con la alerta
    `blob_entries_handed_over:public` (1 entrada).
  - Prueba concreta con `blob_sync_diag` compilado del mismo commit:
    - un blob escrito como root con el SDK nuevo quedó `fluxbee:fluxbee 0640` (prefijo 0750),
      tanto en worker1 como en el motherbee;
    - llegó al otro lado en ~12 s y ~10 s, con el mismo hash;
    - con 0.1.62, el del motherbee habría quedado ilegible para su Syncthing hasta la próxima
      instalación;
    - después se borraron los dos blobs (el borrado llegó a worker1 en ~12 s) y el binario.
- **Rollback:**
  - snapshot `pre-a58-0-1-63`, o `apt install fluxbee=0.1.62`;
  - en un worker que produzca blobs, volver Syncthing a root como dice el HANDBOOK ("El
    rollback").

## 0.1.62 — La réplica de identity ya no cuelga detrás de un firewall (P7-1, A-59); Syncthing vuelve a su binario nuevo (A-57); seguimiento de D32

- **Fecha:** 2026-10-06 (ART) · **Versión anterior:** 0.1.61 · **Commits:** `b6435cf`, `c7c7d6d`,
  `b4868e8` (FINDINGS A-59, A-57, A-54). Tres revisiones adversariales (identity, orquestador, docs),
  más una segunda sobre el arreglo de la carrera de identity.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **sy-identity:**
    - el primario manda un latido en la suscripción cada 20 s sin tráfico;
    - la réplica corta y se vuelve a suscribir tras 60 s sin oír nada;
    - el suscriptor se registra antes de contestar y antes de cada snapshot, así un delta ya no se
      pierde entre el snapshot y el stream;
    - las suscripciones muertas se limpian al registrar una nueva.
  - **Orquestador:**
    - Syncthing se reinicia si cambió su binario o su unit, o si su proceso corre un binario borrado;
    - el update de vendor responde lo que hizo de verdad;
    - el join emite el leaf antes del primer SSH;
    - el revoke borra la llave vieja de recuperación solo por su comentario;
    - el test de guardia de SSH es más estricto;
    - un worker sin vendor falla el join, como ya pasaba en egress e ingress.
  - **Docs:** deriva corregida.
- **Deploy:** build `b4868e8` en 8 min, publish 9 s, deploy 201 s. Snapshots `pre-identity-0-1-62` en
  las 4 VMs (se borró `pre-stage3-0-1-59`). Versiones 0.1.62 en los 4, health `failed=0`.
- **Gate A-59 (P7-1):**
  - Antes (0.1.61), en worker1: la suscripción llevaba 15.929 s sin un paquete en ninguna dirección.
  - Después: tres muestras de `lastrcv` en la suscripción dieron 8 s, 3 s y 18,5 s, con `lastsnd`
    creciendo porque no envía. El canal de publicación sigue su ciclo de 30 s.
  - Corte de 100 s en worker1, con una tabla nft de prueba que descarta lo que llega de
    10.10.10.10:9100 y un temporizador que la borra igual:
    - 60 s después del último latido, la réplica dijo "no frame from the primary for 60s
      (heartbeats stopped); resubscribing";
    - 5 s después de levantar el corte, "identity full sync applied (delta stream subscribed)";
    - la tabla quedó borrada.
- **Gate A-57:**
  - Ningún host corría un Syncthing con el binario borrado, y ninguno lo reinició al arrancar el
    orquestador nuevo.
  - `update category=vendor` a worker1, con el mismo binario 2.0.14, respondió `unchanged: [syncthing]`
    y `restarted: []`. Antes decía `restarted` sin haber reiniciado.
  - El camino con binario nuevo lo ejercita el próximo vendor que cambie.
  - **El reinicio, con una prueba concreta en worker1:**
    - Se instaló una copia idéntica del binario con otro inode. El Syncthing en marcha (pid 2507959,
      arriba desde las 10:21) pasó a correr un archivo `(deleted)` con los mismos bytes: es el caso
      del reinicio perdido de la revisión.
    - El primer `update category=vendor` respondió `unchanged: [syncthing]` y
      `restarted: [fluxbee-syncthing]`. El log dice "restarting syncthing onto its installed binary
      and unit (A-57)": pid nuevo y sano 4 s después.
    - El segundo respondió `restarted: []`, sin bucle.
- **Gate del join (la VM 104 re-clonada):**
  - Se borró y se clonó del template. `add_hive worker2` se lanzó a las 22:50:38, con la caja todavía
    en `starting`.
  - El join esperó en `waiting_for_host` de 22:51:09 a 22:59:35 (cloud-final) y terminó
    `connected` a las 23:01:39.
  - Resultado: `harden_ssh_applied`, `restrict_ssh` `from_only`, `ssh_bootstrap_revoked`, WAN,
    orquestador y dist listos, y el peer de Syncthing enlazado. Ese join ejercitó el leaf emitido
    antes del primer SSH y el envío de vendor que ahora es fatal.
  - En la caja:
    - password apagado;
    - en `authorized_keys` solo quedó la llave descartable del operador;
    - el sudoers del orquestador se borró;
    - estaba el material TLS;
    - en el journal de sshd, las sesiones desde 10.10.10.10 caen solo dentro del join (22:50:44 a
      23:01:27);
    - la suscripción de identity recibía latidos (`lastrcv` 14,8 s);
    - todos los componentes en 0.1.62.
  - Limpieza: llave descartable borrada, `DELETE /hives/worker2` ok por socket, VM 104 apagada.
  - Visto de paso: Syncthing corre como root también en este worker nuevo (A-58, va en 0.1.63).
- **Cierre:** `ops.py health` en `failed=0` desde el deploy, y OPA `in_sync` 4/4.
- **Rollback:**
  - Snapshot `pre-identity-0-1-62`, o `apt install fluxbee=0.1.61`.
  - **Primero los spokes** (`core_rollback` o update al core viejo), después el motherbee: un worker
    de 0.1.62 frente a un motherbee viejo se re-suscribe cada 60 s (HANDBOOK, "El rollback").

## 0.1.61 — SSH solo dentro de `add_hive` (D32, A-54)

- **Fecha:** 2026-10-06 (ART) · **Versión anterior:** 0.1.60 · **Commits:** `31dd105`, `3ae2929`
  (FINDINGS A-54; dos revisiones adversariales).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:** se borraron la reconciliación de TLS por SSH al arrancar y el modo
  `key_only_persist`; un pedido con `ssh_access` se rechaza; el envío del leaf es fatal para el join
  (`TLS_PUSH_FAILED`); el revoke borra una llave de recuperación vieja; un test de guardia recorre el
  código desde cada uso de SSH hasta `accept_add_hive`.
- **Deploy:** build `e0f2d04`, publish 9 s, deploy 198 s; snapshots `pre-ssh-0-1-61` en las 4 VMs
  (se borró `pre-stage2-0-1-58`). Versiones 0.1.61 en los 4, health `failed=0`.
- **Gate D32:** en el journal de sshd de los tres spokes, desde el deploy de 0.1.60 hasta el de 0.1.61,
  3 intentos de SSH desde 10.10.10.10 por spoke (uno por arranque del orquestador del motherbee, todos
  fallidos en la autenticación); desde el deploy de 0.1.61, **cero**.
- **Gate A-52 + D32 con un clon nuevo:** la VM 104 se borró y se clonó otra vez del template;
  `add_hive worker2` se lanzó apenas respondió el guest agent (17:28:37, systemd `starting`).
  - El join entró en `waiting_for_host` a las 17:29:43 con cloud-final `activating` (el dist-upgrade
    del primer arranque) y esperó 8 min; a las 17:37:38 la caja quedó `running` y el join siguió.
  - Terminó `connected` a las 17:39:42: `harden_ssh_applied`, `restrict_ssh`,
    `ssh_bootstrap_revoked`, WAN, orquestador y dist listos; sin `ssh_access` ni
    `spoke_key_vault_ref` en el resultado.
  - Syncthing: dirección estática del motherbee a los 70 s (17:40:38) y opciones de la etapa 3; el
    `posture-check --stage 3` de worker2 da ok (el chequeo "un join da `Static`", movido desde la
    etapa 2, queda hecho).
  - Limpieza: llave descartable borrada (password auth `no`), `DELETE /hives/worker2` ok por socket,
    VM 104 apagada.

## 0.1.60 — Escrituras de root en árboles de `fluxbee`; el join espera a que la caja se asiente (A-51, A-52)

- **Fecha:** 2026-10-06 (ART) · **Versión anterior:** 0.1.59 · **Commits:** `d3710c4`..`05ac354`,
  merge `0af5469` (FINDINGS A-51, A-52; tres revisiones adversariales).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **El `dist/` del motherbee es de root** y Syncthing solo lo lee (`root:fluxbee`, 0750,
    `g+rX,go-w`); los links adentro se borran. Lo hacen el preinst (antes del unpack) y el orquestador
    en cada arranque. `blob/`, el home de Syncthing y el `dist/` de los spokes no cambian.
  - Escrituras de root: temporal `O_EXCL` con token por ejecución y prefijo `.syncthing.`, modo sobre
    el descriptor; directorios administrados abiertos `O_NOFOLLOW` (lo que no es un directorio se deja
    y se reporta, sin tumbar el arranque); el marcador `.stfolder` nunca sigue un symlink.
  - Los joins esperan, antes del sudoers, a que systemd termine de arrancar y a que no corra una etapa
    de cloud-init ni un job diario de apt (hasta 15 min; fase `waiting_for_host`; `HOST_NOT_SETTLED`).
- **Deploy:** build 8 min (`0af5469`), publish 12 s, deploy 192 s; snapshots `pre-hardening-0-1-60`
  en las 4 VMs (se borró `pre-posture-0-1-57`). Los spokes recibieron el core por Syncthing desde el
  `dist/` ya de root: la distribución funciona.
- **Gate:**
  - Motherbee: `dist/` `root:fluxbee 750`, las 107 entradas `root:fluxbee`, 0 links, 0 temporales;
    `blob/*` y `syncthing/` de `fluxbee`; sin errores de Syncthing ni del orquestador.
  - Spokes sin cambios de dueño; `posture-check --stage 3` ok en los 4; OPA in_sync 4/4; versiones
    0.1.60 en los 4; health `failed=0`.
  - A-52 validado con el join de 0.1.61 (clon nuevo, ver arriba).
- **Incidente durante la verificación (mío, resuelto):** un script de verificación corrió
  `sy-orchestrator --version` y `rt-gateway --version` en el motherbee; esos binarios no tienen
  `--version` y arrancaron como segundas instancias.
  - El orquestador de más (~11 min) fue rechazado por el router (UUID ya conectado) y pisó el PID.
  - El router de más (~84 s) recreó los archivos de socket; al matarlo, los paths quedaron muertos
    (los nodos conectados siguieron; uno que se reiniciara no habría podido volver).
  - Arreglo: matar solo esos procesos y reiniciar `rt-gateway` y `sy-orchestrator`. Después: sockets
    ok, PID ok, health `failed=0` en los 4, 15 nodos activos, OPA 4/4, `--stage 3` ok.
  - Regla: una verificación nunca ejecuta binarios del producto; las versiones salen de `/versions` o
    `dpkg-query`.
- **Visto:** en worker1 y egress1 no existe el usuario `fluxbee`: Syncthing corre como root (A-58).

## 0.1.59 — Syncthing sin infraestructura pública (etapa 3 de la postura)

- **Fecha:** 2026-10-06 (ART) · **Versión anterior:** 0.1.58 · **Commits:** `d55ab73`..`f60eb92`
  (FINDINGS A-47; bitácora `2026-10-06`; diseño `docs/host-posture-and-exposure-spec-v1.md` §3.5,
  "Options (stage 3) — as built").
- **Alcance:** motherbee + los tres spokes (core-update); `worker2` (VM 104) saltó de 0.1.57 a
  0.1.59 en la prueba del gate.
- **Qué cambió:**
  - **El orquestador es dueño de las opciones de Syncthing:** sin announce global ni local, sin
    relays, NAT, STUN, reportes de uso ni de crashes, sin auto-upgrade; reconexión cada 10 s; un
    solo listen (el motherbee `tcp://:22000`, los spokes `tcp://127.0.0.1:22000`). Con eso
    Syncthing no abre ningún socket UDP.
  - **El cambio es de una sola vía y ordenado en el código:** cada spoke cambia cuando tiene al
    motherbee en dirección fija y alcanzable (hechos persistidos en
    `/var/lib/fluxbee/orchestrator/syncthing-posture.json` y publicados en `/versions`); el
    motherbee, cuando todos los spokes registrados lo reportan, y mientras tanto nombra a quién
    espera.
  - **Joins protegidos:** mientras corre un `ADD_HIVE_FINALIZE`, el spoke no reinicia Syncthing, y
    el sondeo de convergencia aguanta un reinicio.
  - Dos revisiones adversariales del código antes del release.
- **Build:** 6,5 min (commit `f60eb92`). **CI:** `rust-tests`, `posture-guards` y
  `router-dispatcher-guards` en verde. **Publish:** 9 s, 59 paquetes. **Deploy:** 198 s con
  `ops deploy`; snapshots `pre-stage3-0-1-59` en las 4 VMs (se borró antes `pre-root-io-0-1-56`).
- **Verificación en vivo (gate de la etapa 3):**
  - **Spokes de PROD:** cambiaron solos a los ~3 min del deploy (ingress1 10:21:19, egress1
    10:21:26, worker1 10:21:30 UTC), con un reinicio de Syncthing cada uno.
  - **El motherbee esperó a `worker2`**, nombrándolo en su journal ("no answer: … not reachable
    in LSA"), con discovery prendido.
  - **Salteo de versiones:** `worker2`, apagada desde 0.1.57 con `dynamic` de los dos lados,
    arrancó, se conectó por discovery, recibió el core nuevo y se actualizó de 0.1.57 a 0.1.59 de
    una (10:24:40). Su primer round puso al motherbee en dirección fija y cambió las opciones. El
    motherbee registró el cambio en su siguiente sondeo (10:24:54) y el watchdog escribió sus
    opciones y reinició Syncthing una vez (10:25:52).
  - **`lab/posture-check.py -- --stage 3`:** `verdict ok` en los 5 hives: opciones exactas,
    discovery apagado, ningún socket UDP de Syncthing, el motherbee escuchando en `*:22000`, los
    spokes en `127.0.0.1:22000`, todos conectados al motherbee por `tcp://10.10.10.10:22000`,
    completion 100%.
  - **Reconexión:** tras reiniciar el Syncthing del motherbee, los 4 spokes volvieron en 10,5 s.
  - **Publicación de dist:** un archivo de prueba en `dist/vendor` llegó a los 4 spokes en ~1 s
    después del scan, y su borrado en ~3 s.
  - **OPA** `in_sync` 4/4; **health** desde las 10:18 UTC: 0 unidades caídas, 0 denegaciones, 0
    descartes de origen; `ops versions`: los 5 en 0.1.59, manifest `d0803b54ab5caf58`.
  - **Sin loop de reinicios:** el uptime de Syncthing crece en tres muestras cada ~130 s en los 5
    (motherbee 69 → 203 → 331 s; worker2 218 → 348 → 480 s; el resto igual).
- **Rollback:** `apt install fluxbee=0.1.58` en el motherbee + core-update de los spokes, o el
  snapshot `pre-stage3-0-1-59`. Volver a 0.1.58 no revierte las opciones (nadie las toca) y los
  links siguen funcionando: los spokes discan fijo.

## 0.1.58 — Syncthing: los spokes discan al motherbee y el motherbee solo acepta (etapa 2 de la postura); `blob/active` solo para workers

- **Fecha:** 2026-10-06 (ART) · **Versión anterior:** 0.1.57 · **Commits:** `0a35329`..`9d9391b`
  (FINDINGS A-47, A-50, A-51, A-52, A-53; bitácora `2026-10-06`; diseño
  `docs/host-posture-and-exposure-spec-v1.md`, revisión 8, §3.5 "as built").
- **Alcance:** motherbee + los tres spokes (core-update). `worker2` (VM 104), unida hoy en 0.1.57
  para la prueba de salteo de versiones de la etapa 3, quedó **apagada y afuera del deploy** a
  propósito.
- **Qué cambió:**
  - **Direcciones (A-47, etapa 2):** el watchdog reconcilia cada ~60 s. En cada spoke, el device
    del motherbee pasa a `tcp://<IP del uplink>:22000`. En el motherbee, los devices de los hives
    quedan `dynamic` (solo acepta), y su device se identifica por el ID que registra cada entrada.
    El ID del egress, que nunca se había registrado, se completa por nombre.
  - **A-50:** el motherbee le compartía `blob/active` al egress. El join solo lo comparte con
    workers, y la reconciliación saca a cada spoke de las carpetas que su rol no debe tener (regla
    por rol, nunca por flags). Las carpetas nuevas nacen sin miembros.
  - **Escritor único de `config.xml`**, endurecido contra el directorio del usuario de Syncthing
    (sin seguir symlinks, temporal `O_EXCL`, dueño y modo por descriptor). Guard de CI.
  - **A-53:** el ID local sale de `syncthing device-id` (v2 no tiene `--device-id`).
  - Huérfanos: se borran solo con el registro completo y todos los hives en reposo y registrados.
  - Un join de ingress espera 90 s a que el spoke disque.
  - Cuatro revisiones adversariales del código antes del release.
- **Build:** 7 min (commit `9d9391b`). **CI:** `rust-tests`, `posture-guards` y
  `router-dispatcher-guards` en verde. **Publish:** 10 s, 58 paquetes. **Deploy:** 202 s con
  `ops deploy`; snapshots `pre-stage2-0-1-58` en las 4 VMs (se borró antes `pre-root-tenant-0-1-55`).
- **Verificación en vivo (gate de la etapa 2):**
  - **Primer round, como se predijo:** en el motherbee (08:51:10 UTC) ingress1 pasó a `dynamic`,
    egress1 salió de `fluxbee-blob`, su device ID quedó registrado y Syncthing se reinició **una**
    vez; en worker1 (08:51:47) el motherbee pasó a `tcp://10.10.10.10:22000`, con un reinicio.
    ingress1 y egress1 no cambiaron (Syncthing con 461 k segundos arriba).
  - **`lab/posture-check.py -- --offline worker2`:** `verdict ok` en los 4. Completion 100% en el
    motherbee para los tres spokes de PROD; la oferta pendiente de `blob/active` desapareció del
    egress; las 4 entradas del registro tienen device ID.
  - **Listen:** `default` en los 4, sin cambios. **OPA:** `in_sync` 4/4. **Avisos:** cero líneas
    WARN de Syncthing en los 4 orquestadores desde el deploy.
  - **Sin loop de reinicios:** el uptime de Syncthing crece en tres muestras cada ~130 s
    (motherbee 99 → 228 → 359 s; worker1 64 → 194 → 326 s; ingress1 y egress1 sin reiniciarse).
  - **Health** desde las 08:48 UTC: 0 unidades caídas, 0 denegaciones, 0 descartes de origen en
    los 4. `ops versions`: los 4 en 0.1.58, manifest `043d3d6ac6f51896`.
- **Rollback:** `apt install fluxbee=0.1.57` en el motherbee + core-update de los spokes, o el
  snapshot `pre-stage2-0-1-58`. Los cambios en `config.xml` (direcciones, membresía del egress) no
  rompen 0.1.57: no los revierte nadie y siguen siendo correctos.

## 0.1.57 — Archi y el admin solo en loopback (etapa 1 de la postura), sin botón de apagado en el motherbee, logs sin colores

- **Fecha:** 2026-10-06 (ART) · **Versión anterior:** 0.1.56 · **Commits:** `d0c957d`..`a703b89`
  (FINDINGS A-31, A-45, A-46, A-48; bitácora `2026-10-06`; diseño
  `docs/host-posture-and-exposure-spec-v1.md`, revisión 5).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **Etapa 1 de la postura (A-46):**
    - `architect.listen` pasa a `127.0.0.1:3000` en el example y en el config de dev;
    - el postinst corre `fluxbee-migrate-config`, que mueve a loopback el `0.0.0.0:3000` que
      traían los `hive.yaml` instalados;
    - el orchestrator chequea cada ~60 s si Archi o el admin escuchan fuera de loopback, y deja
      una alerta de drift `posture` por proceso;
    - docs y firstboot apuntan a loopback más túnel SSH;
    - guard de CI en cada push.
  - **El motherbee rechaza `ADD_HIVE_FINALIZE` y `REMOVE_HIVE_CLEANUP`** (`FORBIDDEN`): eran un
    botón de apagado que cualquier orquestador, incluido el del ingress, podía apretar (A-48,
    adelantado; DTAP ronda 3).
  - **A-31:** colores ANSI en los logs solo con terminal.
  - **io.web en la lista de IO del core** (tenant raíz).
  - **Se borró el laboratorio Docker** (no se usaba) y su quickstart del sitio. El sitio cambia
    recién con el merge a `main`.
- **Build:** 20 min (commit `a703b89`). **Publish:** 10 s, 57 paquetes. **Deploy:** 218 s con
  `ops deploy`; snapshots `pre-posture-0-1-57` en las 4 VMs (se borró antes `pre-gov-0-1-54`).
- **Verificación en vivo (validación de la etapa 1):**
  - **Migración:** el `hive.yaml` del motherbee quedó en `listen: "127.0.0.1:3000"`, y el postinst
    dejó su línea en el log de apt.
  - **Listeners:** `ss` en el motherbee muestra 3000 (sy-architect) y 8080 (sy-admin) solo en
    `127.0.0.1`.
  - **Puertos:** `lab/posture-probe.sh 10.10.10.10 3000 8080 9000` desde worker1, ingress,
    egress y fb-build dio `refused`, `refused`, `open` en los 4. En 0.1.56 era `open`, `refused`,
    `open`: **el ingress ya no llega a Archi**.
  - **Local:** en el motherbee, `curl 127.0.0.1:3000/` y `curl 127.0.0.1:8080/hives` dan 200.
  - **Control positivo:** un listener falso llamado `sy-admin` en `10.10.10.10:39555` levantó la
    alerta `loopback_service_exposed_sy-admin` a los 33 s. Es la única alerta `posture`: Archi
    no dio ninguna después del deploy.
  - **A-45 medido por primera vez.** Caída de cada servicio del motherbee en el upgrade:

    | Servicio | 0.1.55 | 0.1.57 |
    |---|---|---|
    | rt-gateway | 70 s | 0 s |
    | sy-vault | 78 s | 1 s |
    | sy-storage | 84 s | 5 s |
    | sy-identity | 86 s | 5 s |
    | resto | — | 0 s |

    Solo paran los que escriben el manifest: el orchestrator estuvo 44 s caído, y el admin hasta
    el reinicio ordenado.
  - **A-31:** cero códigos ANSI en los procesos nuevos de los 4 hosts. Antes del upgrade había
    cientos por servicio en el motherbee. Las únicas líneas con color son los "SIGTERM received"
    de los procesos viejos.
  - **Los 4 hives en 0.1.57** (hash `9e13e6bf…`): el motherbee por dist, los spokes por hash
    instalado. Health con 0 fallas, 0 denials y 0 srcdrops.
  - **CI:** `rust-tests`, `posture-guards` (primera corrida) y `router-dispatcher-guards` en verde
    en `a703b89`.
- **Sin probar en vivo:** el rechazo de las acciones solo-spoke en el motherbee. Lo cubre su test
  unitario: no hay forma limpia de mandarlas como si fuera el orquestador del ingress.

## 0.1.56 — una conexión por UUID, upgrades sin apagar el core, solo io.cloud e io.blob en el tenant raíz

- **Fecha:** 2026-10-03 (ART) · **Versión anterior:** 0.1.55 · **Commits:** `068014d`..`715143a`
  (FINDINGS A-16, A-43, A-44, A-45, B-16; bitácora `2026-10-03`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Antes del deploy, a mano en PROD (el operador, porque el clasificador de permisos se lo
  bloqueó a la sesión):** se purgaron `IO.api@motherbee` e `IO.wapp.default@motherbee`, con sus
  ILKs y sus archivos UUID. Sus configs quedaron guardadas fuera del repo; solo tenían `_system` y
  `tenant_id`.
- **Qué cambió:**
  - **A-44:**
    - el router acepta una sola conexión viva por UUID, con 1 s de espera para los duplicados
      pasajeros;
    - toda conexión que termina se limpia, incluso por error (antes quedaba registrada y sin
      lectura).
  - **A-45:** un upgrade solo para a los que escriben el manifest de runtimes. El orchestrator
    nuevo reinicia en orden lo que corre binarios reemplazados, y needrestart deja el core al
    orchestrator.
  - **A-43:** solo io.cloud e io.blob corren en el tenant raíz y arrancan por defecto
    (`TENANT_ROOT_NOT_ALLOWED`).
  - **A-16:** el watchdog revierte los cambios locales de las carpetas receive-only.
- **Build:** 20,5 min. **Publish:** 11 s, 56 paquetes. **Deploy:** 214 s con `ops deploy`;
  snapshots `pre-root-io-0-1-56` en las 4 VMs (se borró antes `pre-ai-generic-0-1-53`).
- **Incidente del deploy (B-16):**
  - Los core updates salieron antes de que el router del motherbee tuviera de vuelta a los spokes.
    Los tres dieron `TRANSPORT_ERROR` y no llegaron.
  - Igual `ops.py` los dio por actualizados: juzgaba por el hash de dist, no por el instalado.
  - El chequeo de binarios lo mostró. El update se volvió a mandar a mano a las 06:39:52 UTC y
    `ops.py` quedó corregido.
- **Verificación en vivo:**
  - **U-8b validado:** los tres spokes contestaron `status=ok phase=restarting` (9, 5 y 4
    componentes) y terminaron en `core.last_update` `done ok`. worker1 reinició los servicios en
    orden y después se reinició a sí mismo.
  - Los 4 hives en 0.1.56: el motherbee por dist y los spokes por hash instalado (`3260c886…`).
    Ningún binario `(deleted)` y 0 líneas ERROR, en los 4.
  - **A-39 sigue bien:** IO.blob e IO.cloud arrancaron una sola vez, en 0.1.56, con cero
    `203/EXEC`. Son los únicos nodos administrados que quedan en el motherbee.
  - **A-44:** 0 HELLO rechazados, 0 conexiones terminadas por error y 0 handshakes fallidos en el
    tráfico normal de los 4 hives. Los nodos Go arrancaron bien con su doble HELLO.
  - **A-43:** un `run_node` de `IO.api.a43probe@motherbee` en el tenant raíz dio
    `TENANT_ROOT_NOT_ALLOWED`, sin dejar nada creado (ni ILK, ni archivo UUID, ni unit).
  - **A-45:**
    - el `prerm` nuevo y la conf de needrestart quedaron instalados;
    - este upgrade todavía paró todo el core, entre 06:33:26 y 06:34:58, porque corre el `prerm`
      de 0.1.55;
    - se mide en el próximo upgrade.
  - **A-16, validado después del deploy con el OK del operador:** un archivo de prueba en
    `dist/vendor` de worker1 se revirtió en 16 s, con su WARN.
  - **Identity y frontdesk:** 0 errores. La réplica de worker1 hizo el full sync después de su
    reinicio.
  - **CI:** `rust-tests`, `admin-catalog-guard` y `router-dispatcher-guards` en verde en `715143a`.
- **Error propio, sin consecuencias:**
  - Para leer la versión corrí `/usr/bin/sy-orchestrator --version` en worker1. El binario no
    tiene ese flag y arrancó como orchestrator.
  - Murió antes de conectarse al router: no hubo registro nuevo en el router de worker1 y el
    orchestrator real siguió accesible.
  - No hay que correr binarios del core para consultar versiones; se usa `/versions`.
- **Rollback:** snapshot `pre-root-io-0-1-56` o `apt install fluxbee=0.1.55` (los spokes, por core
  update o `core_rollback`).

## 0.1.55 — nadie se registra en el tenant raíz, U-8b, io.slack sin instancia de base

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.54 · **Commits:** `a8c6349`..`975fd34`
  (FINDINGS A-37, A-40, A-43, A-44, A-45; PENDING-BUGS U-8b; bitácora `2026-10-02`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Antes del deploy, a mano en PROD (pedido del operador):** se borró `IO.slack.default@motherbee`
  con purga; corría en el tenant raíz y no tenía su ILK (A-40). Su config quedó guardada fuera del
  repo.
- **Qué cambió:**
  - **Tenant raíz:** nadie se registra ahí. SY.identity lo rechaza para cualquier llamador
    (`TENANT_ROOT_NOT_REGISTRABLE`), el frontdesk responde `TENANT_NOT_REGISTRABLE` sin LLM y
    `register_human` lo rechaza antes de provisionar.
  - **U-8b:** el core update de un spoke contesta `phase: restarting` antes de reiniciar, y deja el
    resultado en `/versions` como `core.last_update`.
  - **io.slack:** sin instancia de arranque; cada binding lo lanza su tenant.
  - **Gate de frontdesk de io.slack/io.wapp:** el handoff va como mensaje `user`; como `data`, el
    frontdesk lo ignoraba.
  - **SHM del router (A-44):** un nodo que se registra de nuevo conserva su slot, y el lector ya no
    pierde el último nodo cuando hay un hueco.
  - **Lab:** los e2e de identity crean sus tenants por SY.admin.
- **Build:** 17,5 min. **Publish:** 10 s, 55 paquetes. **Deploy:** 233 s con `ops deploy`;
  snapshots `pre-root-tenant-0-1-55` en las 4 VMs (se borró antes `pre-cleanup-0-1-52`).
- **Verificación en vivo:**
  - Los 4 hives en 0.1.55 con el mismo manifest (`852a21cd…`). Ningún binario `(deleted)` y 0
    líneas ERROR desde el install, en los 4.
  - **A-39 sigue bien:** IO.api, IO.blob, IO.cloud e IO.wapp.default arrancaron una sola vez, en
    0.1.55, con cero `203/EXEC`; el orchestrator los reapuntó.
  - **Identity y frontdesk:** 0 errores. La réplica de worker1 hizo el full sync y se suscribió a
    los deltas.
  - **U-8b, todavía no:** los spokes recibieron el update con el orchestrator 0.1.54, así que
    ingress1 y egress1 dieron `TIMEOUT` como antes (worker1 contestó a tiempo).
    `core.last_update` está en `/versions`, en `null`. La respuesta antes del reinicio se ve en el
    próximo release.
  - **No probado en vivo:** el bloqueo del tenant raíz, que necesitaría registrar a alguien en PROD.
    Lo cubren los tests y el e2e `identity_negative`.
  - **Visto al verificar (A-45, preexistente):** el upgrade del motherbee deja todo el core caído
    70–86 s, porque el `prerm` para los servicios antes de desempaquetar. Las réplicas lo muestran
    como ~2 avisos de reconexión por segundo.
  - **CI:** `rust-tests` y `router-dispatcher-guards` en verde en `975fd34`.
- **Rollback:** snapshot `pre-root-tenant-0-1-55` o `apt install fluxbee=0.1.54`.

## 0.1.54 — upgrades sin caída de los nodos administrados, el camino .gov cerrado, cognition A-38

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.53 · **Commits:** `7c9f781`..`6aa4ee4`
  (FINDINGS A-37, A-38, A-39, A-41; bitácora `2026-10-02`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Antes del deploy, a mano en PROD (pedido del operador):**
  - se borró `AI.chat@motherbee` con purga (instancia e ILK);
  - se le quitó a IO.slack.default su `io.dst_node`, que apuntaba a AI.chat (config v4);
  - se borraron 12 archivos `.uuid` de nodos ya borrados.
- **Qué cambió:**
  - **A-39:** el arranque del orchestrator relanza en la versión nueva a los nodos que siguen a
    `current`, y `needrestart` ya no reinicia las units `fluxbee-node-*`.
  - **A-41:** la purga de un nodo borra su UUID.
  - **A-37, .gov:**
    - el registro va al tenant del caso y el frontdesk no crea tenants;
    - merge por email: completa solo campos vacíos y mueve los canales;
    - `TENANT_SUSPENDED`;
    - `register_human` informa el ILK final.
  - **A-38, cognition:** `context_close`/`reason_close` y lo reciente primero en la región de memoria.
- **Build:** 13,5 min. **Publish:** 11 s, 54 paquetes. **Deploy:** 231 s con `ops deploy`; snapshots
  `pre-gov-0-1-54` en las 4 VMs (se borró antes `pre-query-decode-0-1-51`).
- **Verificación en vivo:**
  - Los 4 hives en 0.1.54 con el mismo manifest (`7b87d5c1…`). Ningún binario `(deleted)`, 0 líneas
    ERROR desde el install y 0 rechazos del gate y descartes por `routing.src`.
  - **A-39 validado:**
    - los cinco nodos administrados arrancaron una sola vez, en 0.1.54, con cero `203/EXEC`;
    - el orchestrator los reapuntó ("running node follows 'current' and the pointer moved");
    - `/etc/needrestart/conf.d/fluxbee.conf` quedó instalado.
  - **Cognition:** rechazó los umbrales viejos de su config persistida (warning) y aplica los
    defaults; CONFIG_GET informa `context_close`/`reason_close` 0,25, el mismo efecto que antes.
  - **Identity y frontdesk:** 0 errores desde el install.
  - **CI:** `rust-tests` y los guardianes en verde en `6aa4ee4`.
- **Rollback:** snapshot `pre-gov-0-1-54` o `apt install fluxbee=0.1.53`.

## 0.1.53 — limpieza de código muerto: la clave general de IA, sin instancia AI de base, privacidad y cognition

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.52 · **Commits:** `8c8c0fa`..`26edcd5` (ocho,
  uno por tema; bitácora `2026-10-02`, FINDINGS A-32 a A-38).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - **ai.generic** toma la clave general del proveedor del hive en SY.vault (decisión D4 del
    operador). `behavior.vault_key` se rechaza (A-35). Sale el modo gov que quedó del split de
    frontdesk.
  - **La instalación base** ya no levanta `AI.chat@motherbee`. Todo apunta a `ai.generic`: admin,
    Archi, semillas del cookbook y docs (A-36).
  - **SY.frontdesk.gov** sin datos personales en los logs (A-32). Los rechazos de identity conservan
    su código y solo lo transitorio es reintentable (A-33).
  - **SY.identity:** no loguea la dirección en `ILK_PROVISION` ni devuelve el DETAIL de Postgres (A-32).
  - **SY.cognition:** el arranque en frío corre después de esperar al vault y reconstruye con las
    claves del camino en vivo; el estado queda acotado y los scopes se cortan (A-34).
  - **Orchestrator:** 9 funciones muertas menos. Warnings del workspace: 0.
  - **CI:** las actions sobre Node 24.
- **Build:** 14 min. **Publish:** 10 s, 53 paquetes. **Deploy:** 223 s con `ops deploy`; snapshots
  `pre-ai-generic-0-1-53` en las 4 VMs (se borró antes `pre-config-routes-0-1-50`).
- **Verificación en vivo:**
  - Los 4 hives en 0.1.53 con el mismo manifest (`dc605aa6…`). Ningún binario `(deleted)`, 0 líneas
    ERROR desde el install y 0 rechazos del gate y descartes por `routing.src`.
  - **Cognition:** el rebuild de arranque corrió por primera vez (`trigger="startup"`, después de
    esperar al vault; 0 hilos, PROD no tiene datos). CONFIG_GET informa `storage.db_configured: true`.
  - **AI.chat:** arranca en FAILED_CONFIG con *"config.json rejected… send a valid CONFIG_SET"*,
    porque su config tiene `vault_key`. Es lo esperado (A-36).
  - **SY.frontdesk.gov:** arranca y queda degradado (Unconfigured): el vault de PROD no tiene clave
    de IA. El handoff estructurado funciona sin ella.
  - **CI:** `rust-tests`, `go-tests` y los dos guardianes en verde en `26edcd5`.
- **Visto al validar (A-39, preexistente):** los seis nodos administrados de motherbee quedaron
  entre 20 y 80 s en loop `203/EXEC` durante el update, porque el directorio de su versión vieja se
  borró antes de que la unit apuntara a la nueva.
- **Rollback:** snapshot `pre-ai-generic-0-1-53` o `apt install fluxbee=0.1.52`.

## 0.1.52 — limpieza de warnings sin cambio de comportamiento; primer deploy con `lab/ops.py`

- **Fecha:** 2026-10-02 (ART) · **Versión anterior:** 0.1.51 · **Commit:** `0670184` (el código
  es `e1f0323`; el resto son herramientas y documentos del lote de temas chicos, bitácora
  `2026-10-02`).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Nada de comportamiento: fuera campos que nadie leía, accesores de SHM sin uso e imports; tres
    costuras de test pasan a `cfg(test)`. Warnings del workspace raíz: 64 → 31 (lo que queda es
    código muerto que espera la aprobación del operador). El layout de la región de identity da el
    mismo `total_len` en el SDK y en el router.
  - Fuera del `.deb`: `lab/ops.py` (build, publish, deploy) y el publish incremental (`fcb5af6`).
- **Build:** 21 min con `ops build 0.1.52 --wait`. **Publish:** 9 s, 52 paquetes.
- **Deploy:** `ops deploy 0.1.52 --snapshot pre-cleanup-0-1-52 --drop pre-architect-fixes-0-1-49`,
  226 s de punta a punta: snapshots en las 4 VMs, install en motherbee a las 10:24 UTC, spokes en
  paralelo, health.
- **Verificación en vivo:**
  - Los 4 hives en 0.1.52 con el mismo manifest (`250e0aca…`). Ninguna unit corre un binario
    `(deleted)` y todas reiniciaron después del install, salvo `fluxbee-syncthing` (el core-update
    no la toca).
  - 0 líneas ERROR en los journals de las 4 VMs desde el install. Los WARN son de los reinicios
    (RPC y WAN que reconectan) y los ya conocidos (io_slack sin credenciales,
    `EGRESS_MOTHERBEE_BYPASS`).
  - Identity: la réplica de worker1 reintentó mientras el identity de motherbee reiniciaba
    (10:25:21–10:27:03 UTC). Después aplicó el full sync, se suscribió al delta stream y resolvió
    su ILK.
  - OPA: los 4 hives `in_sync` y los routers `ok` (sin policy de usuario cargada). La policy de
    sistema autorizó los core-updates.
  - 0 rechazos del gate y 0 descartes por `routing.src`.
  - CI: `rust-tests`, `admin-catalog-guard` y `router-dispatcher-guards` en verde en `0670184`.
- **Rollback:** snapshot `pre-cleanup-0-1-52` o `apt install fluxbee=0.1.51`.

## 0.1.51 — el admin decodifica la query; SQLite espera el lock en todas las conexiones

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.50 · **Commits:** `9f9982a` (admin) y
  `f7c2520` (sy-timer, wf-generic) (FINDINGS A-30, A-29). La primera build de 0.1.51 (solo con el
  admin) se rehízo antes de publicarla para sumar el arreglo de SQLite.
- **Alcance:** motherbee + los tres spokes (core-update); wf-generic llega como runtime
  `wf.engine/0.1.51`.
- **Qué cambió:**
  - El admin decodifica `%xx` en la query (`+` queda literal).
  - sy-timer y wf-generic aplican sus pragmas de SQLite en cada conexión (en el DSN), no solo en la
    primera del pool.
- **Build:** 6 min. **Publish:** 51 paquetes. Snapshots `pre-query-decode-0-1-51` en las 4 VMs (se
  borró antes `pre-architect-opa-0-1-48`).
- **Install:** motherbee 00:48 UTC del 02/10; spokes a continuación.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Tap creado y borrado con `match_src=IO.pct-a%40motherbee...` → `ok`, el tap ya no está (en
    0.1.50 respondía `NOT_FOUND`).
  - sy-timer activo en motherbee y worker1; `TIMER_LIST` por el admin responde; 0 errores,
    `locked` o `busy` en sus logs.
  - WF de prueba en worker1: creado, el nodo arranca desde `wf.engine/0.1.51` en ~10 s, abre su
    base y recupera 0 instancias sin errores; borrado y el nodo desaparece.
  - 0 rechazos del gate y 0 descartes por `routing.src`.
  - CI: `go-tests` (con la suite de sy-timer), `rust-tests`, `admin-catalog-guard` y
    `router-dispatcher-guards` en verde en los commits de esta versión.
- **Rollback:** snapshot `pre-query-decode-0-1-51` o `apt install fluxbee=0.1.50`.

## 0.1.50 — config-routes cerrado (VPN por hive) y lo que quedaba de OPA

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.49 · **Commit:** `aa206ac` (FINDINGS
  A-18, A-21; panel DTAP D-12). Cambian el router (`rt-gateway`), el admin y la policy de sistema.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Una sola superficie para rutas, VPN y taps: `/hives/{h}/routes|vpns|taps`. Se borraron
    `/routes`, `/vpns` y `/taps` con `?hive=`.
  - La regla 5 de `system.rego` ya no nombra `SY.config-routes`; `system.wasm` y
    `system_route.wasm` recompiladas.
  - El router relee la config de ruteo en cada latido, con control de versión (D-12).
  - Un router sin policy de usuario reporta `ok`, como después de un clear (A-21).
  - `OPA_RELOAD` ya no se reenvía a todos los nodos locales: solo lo usan los routers.
- **Build:** 23 min (cambió la librería del router: se recompila todo). **Publish:** 50 paquetes.
  Snapshots `pre-config-routes-0-1-50` en las 4 VMs (se borró antes `pre-opa-blockB-0-1-47`).
- **Install:** motherbee 00:04 UTC del 02/10; spokes 00:05–00:07.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Recién arrancados y sin policy, los 4 routers reportan `load=0 ok` (antes `load=1 error`).
  - `/routes`, `/vpns` y `/taps` → `UNKNOWN_ROUTE`; `/hives/{h}/...` responde.
  - Ruta de prueba en worker1 y tap de prueba en el motherbee por las rutas por hive: se listan, y
    cada una aparece en el LSA del otro hive. Borradas al terminar.
  - Apply y clear globales: 4,9 s y 5,0 s, los 4 hives. `OPA_RELOAD` aplicado en cada router y
    ningún "forwarded to local nodes". El refresco en el latido solo registra cuando la config
    cambia (motherbee: 4 actualizaciones en la ventana; logs a ritmo normal).
  - 0 rechazos del gate y 0 descartes por `routing.src`.
  - Visto de paso: un DELETE de tap con `@` codificado como `%40` no encontraba el tap (el admin no
    decodificaba la query). Arreglado en 0.1.51.
- **Rollback:** snapshot `pre-config-routes-0-1-50` o `apt install fluxbee=0.1.49`.

## 0.1.49 — arquitecto: un solo admin, el handbook en el `.deb`, el vault esperado al arrancar

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.48 · **Commit:** `c921d1a`
  (FINDINGS A-24, A-25, A-26). Cambian `sy-architect` y `build-deb.sh`.
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - El snapshot y la consulta `query_hive` del plan compiler le preguntan a `SY.admin@motherbee`
    con el hive como destino (antes, a `SY.admin@<hive>`, que solo existe en el motherbee).
  - El `.deb` instala `/etc/fluxbee/handbook_fluxbee.md`, como `install.sh`.
  - Al arrancar, el arquitecto reintenta leer su clave de IA y la URL de su base de mensajes hasta
    que el vault conteste (como SY.storage y SY.identity), las dos a la vez.
- **Build:** 10 min. **Publish:** 49 paquetes (la espera de `pve.py` cortó por conexión; el repo
  quedó con 0.1.49). Snapshots `pre-architect-fixes-0-1-49` en las 4 VMs (se borró antes
  `pre-opa-blockA-0-1-46`).
- **Install:** motherbee 21:59 UTC; spokes 21:59–22:01.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Handbook en `/etc/fluxbee/handbook_fluxbee.md` (30940 bytes).
  - Arranque del arquitecto: las dos lecturas del vault dieron `vault not reachable yet; retrying` y
    1,2 s después escuchaba con `messages_db_configured=true` (en 0.1.48 arrancaba en `false` y lo
    rescataba el aviso del vault). `ai_configured=false`: el vault de 8.x no tiene clave de IA.
  - El admin del motherbee contesta las cinco lecturas del snapshot para worker1. En ingress1 y
    egress1 fallan `runtimes` y `wf-rules` (por rol): queda anotado en A-24.
  - Apply y clear globales: 5,1 s y 5,3 s, los 4 hives. 0 rechazos del gate y 0 descartes por
    `routing.src`; sin policy de usuario al terminar.
- **Rollback:** snapshot `pre-architect-fixes-0-1-49` o `apt install fluxbee=0.1.48`.

## 0.1.48 — el arquitecto declara la policy OPA global (una por sistema, gana la última)

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.47 · **Commit:** `1486807`
  (FINDINGS A-23). Solo cambia `sy-architect`.
- **Alcance:** motherbee + los tres spokes (core-update; el arquitecto corre solo en el motherbee).
- **Qué cambió:**
  - Una solución declara `desired_state.opa` (una policy, sin hive) en lugar de `opa_deployments`
    por hive. Una policy con `hive` no pasa la validación.
  - El snapshot lee la policy que corre una sola vez, del motherbee. Si el rego es el mismo, no hay
    cambio.
  - Un solo `opa_compile_apply`. `OPA_REMOVE` ahora es `opa_clear`.
  - Si dos soluciones declaran OPA, gana la última. El dueño de la policy que corre es la solución
    cuyo manifest guardado la declaró más recientemente. Una solución solo hace clear si la policy
    sigue siendo suya, y el plan (`user_policy` en la confirmación) dice de quién es la que
    reemplaza.
  - El plan compiler recibe el rego declarado y los pasos OPA del plan quedan fijados a él (rego y
    entrypoint exactos, sin hive).
- **Build:** 12 min. **Publish:** 48 paquetes. Snapshots `pre-architect-opa-0-1-48` en las 4 VMs
  (se borró antes `pre-rule3-0-1-45`).
- **Install:** motherbee 21:19 UTC; spokes 21:19–21:21.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`; el arquitecto activo y `/api/status` ok.
  - La respuesta real de `opa_get_policy` del motherbee, con y sin policy, se capturó y quedó como
    test del parser del snapshot.
  - Apply y clear globales: 6,2 s y 4,9 s, los 4 hives. 0 rechazos del gate y 0 descartes por
    `routing.src`; sin policy de usuario al terminar.
  - **No probado en vivo:** el pipeline completo (el diseñador y el plan compiler son IA, y el
    arquitecto arrancó sin IA: A-26). Cubierto por tests: validación, snapshot, reglas de dueño,
    fijado de pasos y confirmación.
- **Rollback:** snapshot `pre-architect-opa-0-1-48` o `apt install fluxbee=0.1.47`.

## 0.1.47 — OPA, bloque B del panel DTAP: un hive trabado dice por qué, se reparan instalaciones a medias, sin carrera en el estado, sin rego viejo

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.46 · **Commit:** `652a9c9` (panel DTAP
  2026-10-01: P-6, P-7, P-13, P-15, D-9, D-10). Solo cambia `sy-opa-rules` (Go).
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - `get_status` de SY.opa.rules dice qué corre el hive, qué le llegó publicado
    (`published_version`/`published_hash`), qué tiene cargado el router (`region_hash`), si está
    `in_sync`, y si espera algo: `waiting {version, hash, since, reason}`. Al minuto de estar
    trabado lo loguea una vez.
  - Cada chequeo de 5 s (motherbee incluido) reescribe la región del router si no tiene la policy
    instalada (instalación cortada a mitad de camino).
  - El estado tiene su propio lock (antes, `lastError` sin lock entre dos goroutines).
  - Instalar sin rego borra el `policy.rego` que había dejado una policy anterior.
  - Los archivos publicados quedan con el dueño de `dist/policy`, no con un `fluxbee` fijo.
- **Build:** 7 min. **Publish:** 47 paquetes. Snapshots `pre-opa-blockB-0-1-47` en las 4 VMs (se
  borró antes `pre-src-binding-0-1-44`).
- **Install:** motherbee 19:55 UTC; spokes 19:56–19:57.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - Sin policy: los 4 con `in_sync: true` y la región en ceros. Apply global: 4,7 s, los 4 con
    `in_sync: true` y la región igual al hash publicado.
  - En el motherbee, `dist/policy/opa/*` es `fluxbee:fluxbee` y solo queda el wasm vigente. Las
    réplicas no tienen `policy.rego`.
  - Hive trabado, forzado en egress1: Syncthing cortado (nft en :22000), apply de la v18 (12 s,
    `pending: [egress1]`, `unreachable: []`) y el manifest del motherbee copiado a mano en la copia
    local (llega el manifest y no el wasm). Estado: `in_sync: false`,
    `waiting {version: 18, reason: "the wasm the manifest names has not arrived"}`; el log a
    1m0s; al desbloquear, Syncthing reconectó en ~1:40 y egress1 instaló solo
    (`installed after waiting 2m51s`). Revert de la carpeta (`/rest/db/revert`) →
    `receiveOnlyTotalItems: 0`.
  - Con el sync cortado del todo (sin la copia a mano), egress1 se ve `in_sync: true`: compara con
    lo que le llegó. Lo que dice que está atrasado es el `pending` del reporte del admin.
  - Clear final: 5,3 s, los 4 sin policy. 0 rechazos del gate y 0 descartes por `routing.src` (los
    únicos "matches" del journal eran el log del guest-agent con el propio comando de conteo).
  - No probado en vivo (cubierto por tests): la reparación de la región a medias y la carrera
    (`-race`).
- **Rollback:** snapshot `pre-opa-blockB-0-1-47` o `apt install fluxbee=0.1.46`.

## 0.1.46 — OPA, bloque A del panel DTAP: arquitecto, reporte honesto, esperas acotadas, re-publicación, una sola vía de escritura

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.45 · **Commit:** `7371ca1` (panel DTAP
  2026-10-01: D-1…D-8, P-4, P-5, P-12, P-14, A-1…A-3, T-5, T-11, T-13)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - El arquitecto traduce las escrituras OPA globales (`/opa/policy*`, incluido clear) y les da 80 s.
  - El admin espera 10 s las respuestas de los hives (antes 30) y como mucho 5 s el escaneo; si el
    motherbee no contesta el paso que cambia la policy, responde `TIMEOUT` (antes `200 ok`).
  - El reporte espera a todos los hives `connected` del registro (alcanzables o no), marca
    `unreachable` y solo cuenta la respuesta de `SY.opa.rules@<hive>` con el hash anunciado.
  - El motherbee re-publica si el manifest no nombra la policy que corre.
  - `CONFIG_SET` sobre SY.opa.rules es de solo lectura.
- **Build:** 11 min. **Publish:** 46 paquetes. Snapshots `pre-opa-blockA-0-1-46` en las 4 VMs (se
  borró antes `pre-sync-hint-0-1-43`).
- **Install:** motherbee 18:47 UTC; spokes 18:48–18:52.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - `CONFIG_SET` con `compile_apply` → `UNSUPPORTED_OPERATION`; la policy no cambió.
  - Apply y clear globales: ~5 s, los 4 hives del registro en `running_hives`, `unreachable` vacío.
  - Con Syncthing de egress1 cortado (nft en :22000): la escritura volvió en 13,6 s (antes ≥32 s)
    con `pending: [egress1]` y `unreachable: []`; al desbloquear, egress1 se puso al día en 7 s.
  - Manifest del motherbee alterado a mano (hash falso): re-publicado en 5,0 s.
  - 0 rechazos del gate y 0 descartes por `routing.src` en los 4 routers; sin policy de usuario al
    terminar.
  - No probado en vivo (cubierto por tests): la traducción del arquitecto, el `TIMEOUT` del paso de
    cambio y la respuesta `PUBLISH_FAILED`.
- **Rollback:** snapshot `pre-opa-blockA-0-1-46` o `apt install fluxbee=0.1.45`.

## 0.1.45 — el orquestador solo puede mandar lo que reenvía; fuera las señales de config muertas

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.44 · **Commit:** `d6505ff` (FINDINGS A-20)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Regla 3 de la policy de sistema: un `SY.orchestrator` solo puede mandar las 16 acciones que de
    verdad reenvía a otro hive (antes, todas las protegidas salvo edge). Fuera quedan `CONFIG_SET`,
    `CONFIG_GET`, `CONFIG_CHANGED`, `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT` e `INVENTORY_REQUEST`.
  - Borrado lo que nadie usaba: el `CONFIG_CHANGED {node_config}` del orquestador y el flag
    `notify`; el `CONFIG_RESPONSE` extra después de `set_storage`; el handler de CONFIG_CHANGED de
    sy-wf-rules (aplicaba workflows sin chequear origen); la rama de io-slack. CONFIG_CHANGED queda
    con un emisor (SY.admin) y un receptor (SY.opa.rules).
- **Build:** 17 min. **Publish:** 45 paquetes. Snapshots `pre-rule3-0-1-45` en las 4 VMs (se borró
  antes `pre-opa-fixes-0-1-42`).
- **Install:** motherbee 15:22 UTC; spokes 15:25–15:28.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - WF en worker1 (lo que más reenvíos entre orquestadores ejercita): creado en 15 s, nodo HEALTHY;
    borrado limpio en motherbee y worker1 (sin directorio, sin entrada en el manifest, sin unit).
  - Lecturas entre hives (`nodes`, `versions`) en worker1, ingress1 y egress1: OK.
  - **0 rechazos del gate y 0 descartes por `routing.src`** en los 4 routers desde el deploy.
  - OPA: apply, apply y clear `converged` (10 / 6 / 4 s).
- **Rollback:** snapshot `pre-rule3-0-1-45` o `apt install fluxbee=0.1.44`.

## 0.1.44 — un nodo solo manda como sí mismo; fuera el aviso de rutas; OPA más rápido

- **Fecha:** 2026-10-01 (ART) · **Versión anterior:** 0.1.43 · **Commit:** `005d0ad` (FINDINGS A-22,
  A-18; bitácora 2026-09-30)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - Seguridad: el router descarta un frame cuyo `routing.src` no es el UUID con el que ese socket
    hizo HELLO. Antes un nodo que conociera el UUID de otro podía mandar como él (A-22).
  - Se borró el CONFIG_CHANGED que el admin mandaba después de cada alta/baja de ruta, VPN o tap
    (no lo usaba nadie) y el trato especial del router a CONFIG_CHANGED.
  - Después de un aviso de OPA, cada hive re-chequea su copia cada 1 s durante 30 s.
- **Build:** 17 min. **Publish:** ~6 min (44 paquetes). Snapshots `pre-src-binding-0-1-44` en las 4
  VMs (se borró antes `pre-opa-global-0-1-41`).
- **Install:** motherbee 03:03 UTC; spokes 03:05–03:08.
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`; 0 nodos caídos.
  - Probe de suplantación en el motherbee: con el UUID del admin, ahora el router lo descarta
    (`routing.src is not the sending node`) y SY.config.routes no responde nada; con 0.1.43 respondía.
    Ningún descarte de tráfico legítimo en los 4 hives.
  - OPA: apply A, apply B, rollback y clear en ~5 s cada uno (antes 8–10 s), todos `converged`.
  - Rutas sin el aviso: una del motherbee aparece en la región LSA de worker1 a los ~11 s y se va a
    los ~7 s; una de worker1 llega al LSA del motherbee a los ~6 s, sin tocar la config del
    motherbee.
  - Estado final: sin policy de usuario ni rutas de prueba.
- **Rollback:** snapshot `pre-src-binding-0-1-44` o `apt install fluxbee=0.1.43`.

## 0.1.43 — el sync hint acepta una carpeta dist puntual (cierra el escaneo de la policy)

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.42 · **Commit:** `2b3abb2` (FINDINGS A-19)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:** el canal `dist` del sync hint acepta cualquier `fluxbee-dist*`; así anda el
  escaneo de `fluxbee-dist-policy` que pide el admin justo después de publicar una policy.
- **Build:** 9 min. **Publish:** ~6 min (43 paquetes). Snapshots `pre-sync-hint-0-1-43` en las 4 VMs
  (se borró antes `pre-purge-order-0-1-40`).
- **Install:** motherbee 01:43 UTC; esta vez se esperó a que el admin respondiera antes de los
  spokes (01:45–01:48).
- **Verificación en vivo:**
  - Los 4 hives con los mismos binarios; 0 `failed`.
  - apply A (v9) 8,4 s · apply B (v10) 10,5 s · rollback (vuelve a A) 8,4 s · clear 8,3 s: los
    cuatro `converged: true`, con la policy correcta en los 4 hives y sus routers. El motherbee
    escanea ~0,5 s después de aplicar; el resto es la transferencia y el chequeo de 5 s de cada hive.
  - config-routes: alta y baja de una ruta en worker1 (v12, v13) con el motherbee intacto (v8); el
    router de worker1 toma el cambio de su SHM al instante.
  - Estado final: ningún hive con policy de usuario.
- **Rollback:** snapshot `pre-sync-hint-0-1-43` o `apt install fluxbee=0.1.42`.

## 0.1.42 — OPA: escanear al publicar; solo el admin primario escribe

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.41 · **Commit:** `ac301d9` (FINDINGS
  A-19, A-20)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:** el admin le pide al orquestador del motherbee que escanee `fluxbee-dist-policy`
  antes del aviso; SY.opa.rules toma CONFIG_CHANGED solo de `SY.admin@motherbee`;
  SY.config.routes ya no aplica listas que lleguen por CONFIG_CHANGED; la respuesta de
  `POST /opa/policy` dice `compile_apply`.
- **Build:** 8 min. **Publish:** ~9 min (42 paquetes). Snapshots `pre-opa-fixes-0-1-42` en las 4 VMs
  (se borró antes `pre-identity-baseline-0-1-39`).
- **Install:** motherbee 01:16 UTC; spokes 01:18–01:21. Un primer core-update salió sin hash porque
  el admin todavía arrancaba: lo rechazaron, sin efecto.
- **Verificación en vivo:** binarios iguales en los 4 hives, 0 `failed`. **El escaneo no anduvo:** el
  orquestador rechaza `folder_id: fluxbee-dist-policy` (`INVALID_REQUEST`, solo acepta
  `fluxbee-dist`) y sin escaneo el watcher retuvo también el apply: a los 33 s solo el motherbee
  corría la v8, y el clear siguiente se fusionó con él en un único escaneo a los 64 s → se arregla
  en 0.1.43.
- **Rollback:** snapshot `pre-opa-fixes-0-1-42` o `apt install fluxbee=0.1.41`.

## 0.1.41 — una sola policy OPA de usuario en todos los hives, distribuida por Syncthing

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.40 · **Commit:** `72891be` (FINDINGS A-7;
  bitácora 2026-09-30; [`docs/opa-distribution.md`](../docs/opa-distribution.md))
- **Alcance:** motherbee + los tres spokes (core-update) + `hive.yaml` de motherbee, ingress1 y
  egress1 (SY.opa.rules en ingress/egress; receta en HANDBOOK §12).
- **Qué cambió:**
  - SY.opa.rules corre en los 4 hives. Solo el motherbee compila: aplica y publica el wasm en la
    carpeta Syncthing `fluxbee-dist-policy`; los demás hives lo instalan si el sha256 coincide.
  - Las escrituras OPA son globales (`/opa/policy*`); las de un hive puntual se rechazan. La
    respuesta dice qué hives ya la corren y cuáles siguen pendientes.
  - CONFIG_CHANGED viaja por el ruteo normal y cruza hives; se eliminaron
    `PUT /config/routes|vpns|taps`.
- **Build:** 17 min. **Publish:** ~9 min (`dpkg-scanpackages` sobre 41 paquetes). Snapshots
  `pre-opa-global-0-1-41` en las 4 VMs (se borró antes `pre-wf-mirror-0-1-38`).
- **Migración (00:25–00:39 UTC):** `hive.yaml` del motherbee editado antes del `apt install` (backup
  `hive.yaml.pre-opa-0-1-41`); el orquestador armó `dist/core/ingress` con 5 componentes y
  `dist/core/egress` con 4, y el binario llegó a ingress1/egress1 en menos de un minuto; después
  `hive.yaml` de esos dos spokes y core-update a los tres (TIMEOUT como siempre; los binarios
  quedaron con el mismo hash que en el motherbee).
- **Verificación en vivo:**
  - SY.opa.rules `running` en los 4 hives (en ingress1/egress1 la unit la creó el orquestador al
    arrancar). Carpeta `fluxbee-dist-policy`: `sendonly` con 4 devices en el motherbee (heredados
    de vendor) y `receiveonly` con 2 en cada spoke. 0 servicios `failed`.
  - `POST /opa/policy` (v6): `converged: true` en 16 s; los 4 routers con v6 cargada.
  - Ponerse al día: con Syncthing de egress1 cortado (nft temporal en :22000), la v7 respondió
    `pending: [egress1]` y egress1 siguió con su v6; al desbloquear instaló la v7 solo, en 27 s.
  - Clear: el motherbee limpió al instante, pero los spokes tardaron ~63 s (el watcher de Syncthing
    retiene 60 s un cambio hecho solo de renames y borrados) y la respuesta salió con los tres
    pendientes → se arregla en 0.1.42.
  - Escritura para un hive puntual → 400; ruta vieja `/hives/{h}/opa/policy/clear` → 404. En
    ingress1/egress1: `/var/lib/fluxbee/opa` `700`, SHM `600`.
  - Factory reset en dry-run: ahora lista la policy de los 4 hives.
- **Rollback:** snapshot `pre-opa-global-0-1-41`, o `apt install fluxbee=0.1.40` + restaurar
  `hive.yaml.pre-opa-0-1-41` en motherbee, ingress1 y egress1.
  - **Nota (2026-10-01, panel DTAP P-9):** antes del `apt install`, `POST /opa/policy/clear` y
    esperar `converged: true`; si no, la última policy global queda aplicada en los workers y en
    memoria en los routers de ingress/egress. Ver HANDBOOK §12, "El rollback".

## 0.1.40 — purga de WF en el motherbee en orden y tolerante a "ya no existe"

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.39 · **Commit:** `43dd174` (FINDINGS A-15)
- **Qué cambió:** en un worker, wf-rules purga en el motherbee dejando la versión current al final
  (el admin no deja borrarla mientras haya otras) y toma `RUNTIME_NOT_FOUND` /
  `RUNTIME_VERSION_NOT_FOUND` como hecho.
- **Build:** 7 min. **Install:** motherbee 23:03 UTC; spokes 23:05 (solo cambió wf-rules).
- **Verificación en vivo — ciclo completo de un WF en worker1:**
  - crear → nodo HEALTHY en 0.0.1 (18 s: publica, espera la sync, spawnea);
  - re-aplicar con cambio → HEALTHY en 0.0.2 (18 s);
  - borrar → OK; el motherbee queda sin versiones ni entrada en el manifest; el espejo de worker1
    con 0 cambios locales (19/19).
  - Limpieza: `AI.vaultprobe@worker1` borrado con su ILK purgado (el primario vuelve a 24 ILKs,
    como al empezar el día); sin nodos ni scripts de prueba en las VMs.
- **Rollback:** snapshot `pre-purge-order-0-1-40` o `apt install fluxbee=0.1.39`.

## 0.1.39 — identity no borra los ILK de un hive que arrancó sin primario; purga de WF dirigida

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.38 · **Commits:** `2248612`, `85c3244`
  (FINDINGS A-15, A-17). Un primer build de 0.1.39 sin `85c3244` se rehízo antes de publicarse.
- **Qué cambió:** la réplica de identity no publica su conjunto de ILK propios hasta tener una base
  del primario; wf-rules dirige `remove_runtime_version` al motherbee (`target`).
- **Install:** motherbee 22:38 UTC; spokes 22:41 (solo se reiniciaron identity y wf-rules, que eran
  los binarios cambiados).
- **Verificación en vivo:**
  - Réplica de worker1 arrancada con el primario bloqueado (nft temporal): logueó
    `holding its self-owned snapshot` mientras estuvo sin base; al volver, snapshot de 25 ILKs. El
    primario **siguió en 25, con el ILK de `AI.vaultprobe@worker1`** (con 0.1.38 había pasado de
    25 a 24).
  - Workflow en worker1: creado en 3 s, nodo HEALTHY. Delete: el motherbee borró la 0.0.1 y la
    sync limpió worker1 sin cambios locales, pero la purga cortó en la 0.0.2 (`RUNTIME_NOT_FOUND`:
    directorio huérfano) → se arregla en 0.1.40.
- **Rollback:** snapshot `pre-identity-baseline-0-1-39` o `apt install fluxbee=0.1.38`.

## 0.1.38 — wf-rules no toca el espejo `dist/` de un worker; el orquestador no spawnea sin archivos

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.37 · **Commits:** `676732d`, `20baec3`
  (FINDINGS A-15)
- **Qué cambió:** en un hive que no es el motherbee, wf-rules purga pidiéndole
  `remove_runtime_version` al admin en vez de borrar su espejo; el orquestador responde
  `RUNTIME_NOT_PRESENT` hasta que el `package.json` del paquete está en el hive.
- **Build:** 4 min (incremental). **Install:** motherbee 22:07 UTC; spokes 22:10–22:12.
- **Reparación puntual:** espejo `dist-runtimes` de worker1 restaurado con `POST /rest/db/revert`
  de Syncthing (15 → 0 cambios locales). Después `WF.w1probe@worker1` quedó HEALTHY: **el primer WF
  corriendo en un worker**.
- **Verificación en vivo:** el delete dejó el espejo limpio (0 cambios locales), pero el pedido al
  admin salía sin `target` (`INVALID_REQUEST`) → se arregla en 0.1.39.
- **Rollback:** snapshot `pre-wf-mirror-0-1-38` o `apt install fluxbee=0.1.37`.

## 0.1.37 — lo que salió de validar 0.1.36: spawn de WF en workers, veredicto del vault, permisos de OPA

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.36 · **Commits:** `3055a40`, `6c708a6`,
  `c09f9af` (FINDINGS A-12 a A-14)
- **Alcance:** motherbee + los tres spokes (core-update).
- **Qué cambió:**
  - wf-rules espera (hasta 20 s) a que el paquete recién publicado llegue al hive antes de spawnear.
  - SDK: `VaultClient::get`/`list` conservan el veredicto del vault (`KEY_NOT_FOUND` ya no llega
    como error JSON).
  - SY.opa.rules: SHM `0600` (era `0666`), policy `0600`, dirs `0700`.
- **Build:** VM110 `build-deb.sh 0.1.37` (22 min) → publish (37 paquetes). Snapshots
  `pre-followups-0-1-37` en las 4 VMs (se borró antes el más viejo de cada una).
- **Install:** motherbee 21:37 UTC; spokes 21:40–21:42 (TIMEOUT como siempre; `rt-gateway` con el
  mismo hash en los 4 hives).
- **Verificación en vivo:**
  - `/dev/shm/jsr-opa-motherbee` y `…-worker1` → `rw-------`; `/var/lib/fluxbee/opa` → `700`.
  - Workflow en worker1: `RUNTIME_NOT_AVAILABLE` a las 21:43:39/40/42/46 y spawn OK a las 21:43:51,
    cuando llegó el manifest. **El nodo quedó en crash-loop** por archivos del paquete faltantes:
    wf-rules los había borrado del espejo local (A-15) → se arregla en 0.1.38.
- **Rollback:** snapshot `pre-followups-0-1-37` o `apt install fluxbee=0.1.36` + core-update.

## 0.1.36 — config con alcance de hive, control de origen, vault/admin desde workers, identity se pone al día

- **Fecha:** 2026-09-30 (ART) · **Versión anterior:** 0.1.35 · **Commits:** `ad48ecb`, `15d53cc`,
  `d24e3aa`
- **Alcance:** **motherbee + los tres spokes** (core-update a worker1, ingress1 y egress1: cambian
  el router, SY.config.routes, identity, cognition y los nodos Go).
- **Qué cambió** (bitácora 2026-09-30; FINDINGS A-8 a A-11). Los cuatro bugs se confirmaron en
  vivo con 0.1.35 antes de tocar nada:
  - `ConfigChangedPayload.hive`: un `add_route` en worker1 ya no pisa las rutas del motherbee, y
    una operación OPA dirigida a worker1 ya no se ejecuta en el motherbee.
  - CONFIG_CHANGED pasa a ser acción protegida (gate en el router). SY.config.routes y
    SY.opa.rules solo aceptan mutaciones y comandos de `SY.admin@motherbee`.
  - `VaultClient::for_primary` y wf-rules → `SY.admin@motherbee`: AI, cognition, IO y WF en un
    worker llegan al vault y al admin.
  - La réplica de identity se resincroniza con un snapshot en cada suscripción.
- **Build:** VM110 `build-deb.sh 0.1.36` (22 min: cambió el SDK) → publish (36 paquetes).
  Snapshots `pre-config-scope-0-1-36` en las 4 VMs (la más vieja de VM100 se borró antes).
- **Install:** motherbee 20:42→20:43 UTC; core-update de los spokes → `TIMEOUT` como siempre, pero
  aplicado: `rt-gateway` con el mismo hash en los 4 hives y servicios reiniciados 20:44–20:46.
- **Verificación en vivo** (los mismos pasos que confirmaron cada bug):
  - Dos altas en worker1 → el motherbee sigue en 0 rutas; su SY.config.routes loguea
    `config changed addressed to another hive; ignoring`.
  - Nodo falso: CONFIG_CHANGED → `router dropped CONFIG_CHANGED from unauthorized origin`;
    `add_route` directo → `FORBIDDEN`; `rollback_policy` → `UNAUTHORIZED`. Rutas intactas.
  - `node_config` legítimo del orquestador en worker1 → pasa el gate.
  - AI y cognition en worker1 consultan `SY.vault@motherbee` (antes `NODE_NOT_FOUND` contra
    `SY.vault@worker1`).
  - Workflow en worker1 → **publica** (antes `PACKAGE_PUBLISH_FAILED: UNREACHABLE`). El primer
    spawn todavía choca con la sincronización de Syncthing (~11 s): se arregla en 0.1.37.
  - Réplica de worker1 arrancada con el primario inalcanzable (regla nft temporal) → DEGRADED →
    al volver la conexión, `identity full sync applied (delta stream subscribed)` sola.
  - Clear de OPA dirigido a worker1 → el motherbee lo ignora; el admin informa `pending worker1`
    (que llegue a los spokes es el diseño pendiente, A-7).
- **Rollback:** snapshot `pre-config-scope-0-1-36` (las 4 VMs) o `apt install fluxbee=0.1.35` +
  core-update de los spokes.

## 0.1.35 — admin: los orquestadores vuelven a poder marcar el ILK de un nodo (fix de 0.1.34)

- **Fecha:** 2026-09-28 (ART) · **Versión anterior:** 0.1.34 · **Commit:** `5f1cd71`
- **Alcance:** **motherbee** (VM100). Solo cambian SY.admin y el script de reset; los spokes siguen
  con el core de 0.1.34 (el admin no corre en ellos).
- **Qué cambió:** la **regresión de 0.1.34**. Al sumar `delete_ilk` a la superficie de Cloud, el
  gate de relay (EDGE-06) empezó a rechazar el teardown del **orquestador**. El kill con
  `purge_instance` respondía ok pero dejaba el ILK del nodo **activo y huérfano**. Fix: cualquier
  `SY.orchestrator@*` puede usar `delete_ilk` (y solo eso), siguiendo la regla hermana de
  `vault_put`. Además, el reset verifica OPA por resultado, hive por hive.
- **Build:** VM110 `build-deb.sh 0.1.35` (7 min) → publish (35 paquetes).
- **Verificación en vivo:**
  - e2e 27/31. Los 4 FAIL son hallazgos previos y explicados: la réplica rechaza el listado del
    admin y la escritura de OPA no llega a los spokes.
  - Se verificó el kill+purge: el ILK del nodo queda purgado y el nombre libre.
  - **Reset instalado por el `.deb`** con datos reales: 1 nodo, 2 ILKs, 2 secretos, **8 tenants**
    y la política OPA v5, con RC=0 y 0 problemas. El segundo dry-run da vacío.
  - Puerta de Cloud en 200, con 13 acciones.
- **Rollback:** snapshot `pre-teardown-fix-0-1-35` (VM100) o `apt install fluxbee=0.1.34`, que
  re-introduce la regresión.

## 0.1.34 — identity: marcar / restaurar / purgar · `opa_clear` · reset a fábrica completo

- **Fecha:** 2026-09-28 (ART) · **Versión anterior:** 0.1.33 · **Commits:** `13f426f`, `60331c5`
  (más `f47dcb2` y `c1fee0d` del reset).
- **Alcance:** **motherbee + los 3 spokes**, con core update, porque cambian el router,
  SY.identity y SY.opa.rules, que también corren en los spokes.
- **Qué cambió:**
  - **Identity:** borrar un ILK o un tenant solo **marca**. La marca es reversible, oculta el
    registro y deja sus claves reservadas; la **purga** es física y exige la marca. El tenant
    marca en cascada a sus ILKs.
  - **Migración de DB v2.**
  - **Cloud:** marca y restaura, siempre acotado al tenant que reclama; **no puede purgar**.
  - **`opa_clear`:** los routers descargan la política de usuario (antes la ignoraban hasta
    reiniciarse).
  - **`fluxbee-factory-reset`:** viaja en el `.deb`.
- **Build:** VM110 `build-deb.sh 0.1.34` (24 min) → publish (34 paquetes). Snapshots
  `pre-lifecycle-0-1-34` en las 4 VMs.
- **Verificación en vivo:**
  - la migración v2 se aplicó;
  - los spokes hicieron el core update (hash `e67a8815…`) y sus servicios reiniciaron;
  - e2e 26/30 por la puerta pública real: marcar, ocultar, reservar, scope por tenant, cascada,
    restaurar y purgar;
  - el router descargó la política OPA (visto en su log).
- **Regresión encontrada en el e2e:** el teardown del orquestador quedó bloqueado. Se corrigió en
  0.1.35.
- **Rollback:** snapshot `pre-lifecycle-0-1-34` (las 4 VMs) o `apt install fluxbee=0.1.33`. Ojo:
  la migración v2 solo **agrega** columnas, así que 0.1.33 las ignora sin problema.

## 0.1.33 — io.cloud: get_ilk email-SOLO (cross-tenant) — el login del website

- **Fecha:** 2026-08-28 (ART) · **Versión anterior:** 0.1.32 · **Commit:** `01b6266`
- **Alcance:** **motherbee** (VM100). io.cloud + fluxbee_sdk (warn seqlock en el reader de listado) + docs.
- **Qué cambió:** el website (OAuth Google) tiene SOLO el email en el primer login; `params.tenant_id` ahora es
  OPCIONAL con `params.email`. Con tenant: probe O(1) 0.1.32 sin cambios. SIN tenant: scan cross-tenant de los
  canales `cloud` (`list_ich_options_from_hive_id`, propaga errores) → `{exists, ilk: solo si match único,
  matches:[UN subset POR tenant donde existe el email — cada uno con su tenant_id]}`. De `matches` el website
  saca el tenant; 0 matches → create_tenant+register_human; N matches → selector de empresa.
- **Hardening de la review (2 MEDIUM pre-ship):** (1) `tenant_id` PRESENTE pero malformado (número, vacío,
  formato inválido) = error fuerte — nunca ensancha silenciosamente un probe con tenant a scan global (branch
  por presencia de key, no por parse); (2) `matches` garantiza uno-por-tenant: estados transitorios de identity
  (ventana de merge-alias, address takeover) pueden dejar 2 ilks activos en un mismo (cloud,email,tenant) — los
  tenants ambiguos se re-prueban con el resolver O(1) autoritativo (misma respuesta que el probe con tenant).
  + LOW: warn en seqlock-timeout del reader de listado (outages de first-login diagnósticables).
- **Semántica aceptada (documentada):** canales disabled matchean (register_human los crea enabled:false — un
  filtro estricto rompería el first-login); el scan es O(hive) por request (aceptado al tamaño actual; list_ilks
  ya escanea igual). Oráculo email→tenants expuesto SOLO a la clase de caller ya confiada (bearer del edge,
  default-deny sin edge configurado).
- **Build:** VM110 `build-deb.sh 0.1.33` → publish repo :8900 (33 paquetes).
- **Verificación E2E (7/7 desde internet):** login 1-empresa (`ilk` poblado con tenant), register en 2ª empresa,
  login 2-empresas (`ilk:null, matches:[2]` uno por tenant), email desconocido (`exists:false`), `tenant_id:42`
  → error fuerte (no scan), y regresiones email+tenant / ilk_id intactas.
- **Rollback:** `apt install fluxbee=0.1.32` (aditivo; los selectores 0.1.32 no cambiaron).

## 0.1.32 — io.cloud: selector por EMAIL en get_ilk / get_ilk_details

- **Fecha:** 2026-08-27 (ART) · **Versión anterior:** 0.1.31 · **Commit:** `90fd64a` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `io.cloud` + `fluxbee_sdk` (identity readers + catálogo cloud) + docs.
- **Qué cambió:** el Cloud tiene el email del humano (login Google) pero no el `ilk_id`; ambos reads aceptan
  `params.email` + `params.tenant_id` como selector alternativo a `params.ilk_id` (exactly-one, en ambos ops).
  El email ES la dirección del canal `cloud` del ilk (register_human lo provisiona así) → el `get_ilk` local
  resuelve con un probe O(1) del índice SHM `(canal, dirección, tenant)` — patrón io.api inbound; más barato que
  el propio path por id (O(n)). `get_ilk_details` pre-resuelve email→ilk_id local y relaya el `get_ilk` de admin
  sin cambios (admin/identity no ganan selector email; `ILK_NOT_FOUND` sin tocar admin si no matchea).
  `tenant_id` OBLIGATORIO con email (unicidad por `(canal, dirección, tenant)` — el mismo email puede ser dos
  ilks en dos empresas) y validado canónico `tnt:<uuid>` fail-loud. SDK: variantes
  `resolve_identity_option_*_strict` (SHM ilegible = `Err`, nunca miss silencioso — el laundering F-09 es para
  el degrade de io.api, no para un read API autoritativo).
- **Review adversarial (2 lentes) pre-ship:** ambos MEDIUM corregidos ANTES del deploy (semántica strict de SHM;
  exactly-one en el op con PII); oráculo cross-tenant descartado (el tenant integra la key del índice); el LOW de
  request_id era falso positivo (el wrapper del run_loop lo inyecta). Tests: SDK 6/6+6/6, io-cloud 9/9, admin 3/3.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.32` → 245 MB · publish repo :8900 (32 paquetes).
- **Deploy:** reboot (qga caído, patrón conocido) → `apt install fluxbee=0.1.32` → el restart manual de io.cloud
  llegó antes de que el unit existiera, y **el liveness-reconcile de 0.1.30 lo respawneó solo con el binario
  nuevo** (~60s) — el fix anterior auto-desplegó este feature.
- **Verificación E2E (10/10 desde internet):** hit por email (`exists:true` + subset), case-insensitive, miss,
  tenant equivocado → `exists:false` (aislamiento), exactly-one en ambos ops, email-sin-tenant error claro,
  details por email → ficha completa, `ILK_NOT_FOUND` con request_id, y regresión por `ilk_id` OK.
- **Rollback:** `apt install fluxbee=0.1.31` (feature aditivo; el selector por id no cambió).

## 0.1.31 — SDK/rpc: fix del veneno del edge (response_only familia-completa) — el bug real de crearPersona

- **Fecha:** 2026-08-26 (ART) · **Versión anterior:** 0.1.30 · **Commit:** `862c5e4` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `crates/fluxbee_sdk/src/rpc.rs` — linkeado estático en TODOS los binarios (rebuild integral del .deb; hicieron falta reboot para respawnear los runtimes con el SDK nuevo).
- **El bug (prod, determinístico):** `register_human` en io.cloud espera el veredicto del frontdesk con
  `send_with_matcher(frontdesk_reply_matcher())` cuyo success matcher es `any_msg_type(user)` (inevitable: el
  veredicto es un frame `user` con `meta.msg=None`). `send_with_matcher` registraba los success shapes en el
  registro **PERMANENTE** `response_only` (solo-insert, se arma AL ENVIAR). `classify` consulta ese registro por
  FORMA (sin trace) ANTES del catch-all `Any→Command` de io.cloud → tras UN `register_human` (éxito O timeout),
  TODO frame user-kind entrante (todos los Cloud requests del edge) se descartaba como "orphaned response" en
  silencio (debug-level) → edge 504 → Cloud "crearPersona falló (FRONTDESK_REJECTED)". Solo un restart curaba.
  **Probado por 3 vías:** código, panel de 4 mappers, y experimento controlado en prod (baseline 200×3 →
  1 register SUCCESS → probe 6s después timeout permanente). Refuta el framing "presence decay ~6min":
  monitor 12min solo-lists = 36/36 OK; VM100 sana en pleno fallo (cpu 0%, mem 15%) — NO era Proxmox/memoria.
- **El fix:** `register_response_only` saltea `AnyMsgOfType` (misma razón que el skip de `Any`: global drops).
  Exact/OneOf siguen registrándose — protecciones AF-P2b del orquestador intactas. Late replies familia-completa
  quedan cubiertas por pending (trace) + stale table (30s TTL) + gates propios de cada nodo (io.cloud ignora
  src≠edge fail-closed). **Desactiva 3 bombas: io.cloud (detonada), io.api (handoff idéntico) y
  `sy_admin::send_admin_request` (`any_msg_type(admin)`).** Helper muerto removido; test venenoso invertido en
  regresión `family_wide_success_matcher_never_poisons_command_traffic`; doc RPC multiplexing actualizado.
- **Review:** 39/39 tests SDK verdes · adversarial 3 lentes (blast-radius de todos los callers, ventanas de
  protección, coherencia de tests): **0 HIGH / 0 MEDIUM**, solo LOW de corrimiento de métricas/logs.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.31` → 234 MB. **Publish:** `apt-repo-publish.sh` → repo :8900
  (ojo: el server `:8900` es un unit transient `systemd-run` que NO sobrevive reboots de VM110 — hubo que relevantarlo).
- **Verificación en vivo (E2E desde internet, post-reboot):** **4 `register_human` consecutivos** (3 frescos +
  1 repetido idempotente, incluido el flujo real del operador) todos `status=ok success=True reg=complete`, con
  `list_cloud_actions` 200 intercalado tras CADA uno + estabilidad 200×3 — con 0.1.30 el primer register mataba
  todo el tráfico posterior del edge.
- **Rollback:** snapshot `pre-rpc-poison-fix-0-1-31` (VM100) o `apt install fluxbee=0.1.30` + reboot (re-introduce el veneno).

## 0.1.30 — orquestador: self-heal de runtimes managed muertos + adiós `is_packaged_singleton`

- **Fecha:** 2026-08-26 (ART) · **Versión anterior:** 0.1.29 · **Commit:** `af1166a` (branch `daily_onworking_coa`)
  (+ `9e81687` reword docs "singleton"→"managed runtime").
- **Alcance:** **motherbee** (VM100). Toca `sy_orchestrator` + `fluxbee_sdk::managed_node`.
- **El bug (prod):** `IO.cloud@motherbee` fue hallado MUERTO (unit transient inactive) con la MB 19.8 días up y
  nada lo resucitaba: los runtimes managed corren con `systemd-run --property Restart=always`, pero si agotan el
  start-limit nadie los recrea — `reconcile_persisted_custom_nodes` corría SOLO en el bootstrap, y el watchdog de
  5s solo los LOGUEABA ("node disconnected") mientras SÍ reinicia SY.*/rt-gateway → edge 504 hasta el próximo boot.
- **El fix:** `run_managed_runtime_reconcile_loop` — task PROPIA de 60s (NO el hot-path del watchdog de 5s: un
  systemctl lento jamás frena el self-heal de SY.*) que re-corre el reconcile persistido; el reconcile ahora hace
  `try_lock_node` (SO-04) por nodo — si un admin-op tiene el lock, saltea y reintenta el ciclo siguiente. Solo
  revive nodos `relaunch_on_boot=true`. **Nota operativa:** `kill_node` sin purge de un nodo boot ahora revive en
  ≤60s (consistente con SY.*; bajar durable = purge). Además: borrado el mecanismo muerto
  `is_packaged_singleton`/`PACKAGED_SINGLETON_NODES` (lista vacía; el guard de doble-dueño lo sostiene `kind=="SY"`/RT.gateway).
- **Review:** adversarial 3 lentes — el hallazgo mayor (reconcile inline bloqueaba el watchdog) se corrigió
  ANTES del deploy (task propia + try_lock).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.30` → 234 MB. **Publish:** repo :8900.
- **Verificación en vivo:** boot-reconcile relanzó los 9 runtimes (`started=9 skipped=0 failed=0`); el loop
  periódico corre cada 60s exacto (`reconcile completed started=0 skipped=9`); endpoint Cloud 200 post-boot.
- **Rollback:** snapshot `pre-liveness-fix-0-1-30` (VM100) o `apt install fluxbee=0.1.29`.

## 0.1.29 — io.cloud: camino de LECTURA de Cloud (fase 2) — reads rápidos SHM + get_ilk_details relay

- **Fecha:** 2026-08-25 (ART) · **Versión anterior:** 0.1.28 · **Commit:** `a0bc25d` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `fluxbee_sdk::cloud` + el nodo runtime `IO.cloud` + el nodo core `SY.admin`.
- **Qué cambió:** le da a Fluxbee Cloud una forma de **leer** lo que creó (patrón io.api: existencia/subset desde la
  SHM local sin round-trip; data completa por relay a admin).
  - **SDK (single source):** `CLOUD_LOCAL_OPS += get_ilk/get_tenant/list_ilks` (reads SHM que io.cloud sirve solo);
    `CLOUD_OP_ACTIONS += get_ilk_details → get_ilk` (relay). `CLOUD_EXPOSED_ACTIONS` gana `get_ilk` → admin
    `authorize_cloud_relay` + el catálogo Cloud lo auto-permiten/publican (advertised==enforced). `cloud_action_catalog` documenta las 4.
  - **io.cloud:** `handle_shm_read` lee la SHM de identidad vía `fluxbee_sdk::identity` (`list_ilks_from_hive_id`/
    `tenant_exists_in_hive_id`, por `config.hive_id`) — los mismos readers de io.api, **sin round-trip**. Subset
    `{ilk_id, ilk_type, registration_status, tenant_id, display_name}`. `get_ilk_details` → admin `get_ilk` (identification PII + canales + tenant).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.29` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo (E2E desde ingress):** las **4 ops OK** — `get_ilk` `{exists,ilk:subset}`; `get_tenant`
  `{exists,ilk_count:4}`; `list_ilks` `{count:4, ilks:[…pepito…]}`; `get_ilk_details` registro completo
  (`identification{email,phone,company,attributes}` + `channels` + `tenant{pepito}`). SDK 5/0 · io.cloud 9/0 ·
  admin catalog test actualizado. **0 units failed**, io.cloud `NRestarts=0` (un 502 transitorio inicial por el ICH
  re-registrándose tras el restart). **Contrato para Cloud dev en `docs/io-cloud-api.md` §4.6–4.9.**
- **Nota (owner-deferred):** ownership de tenant sigue MVP-trusted — un holder del bearer puede leer cualquier id
  que nombre (misma postura que las escrituras).
- **Rollback:** snapshot VM100 `pre-cloud-readpath-0-1-29`, o `apt-get install -y --allow-downgrades fluxbee=0.1.28`.

---

## 0.1.28 — frontdesk: reconciliar CONFIG_GET/CONFIG_SET con el modelo autónomo (Model D', como architect)

- **Fecha:** 2026-08-25 (ART) · **Versión anterior:** 0.1.27 · **Commit:** `1438737` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `SY.frontdesk.gov` (`ai_node_runner`).
- **Qué cambió:** un panel de 4 agentes confirmó que architect Y el executor de admin son autónomos **y
  MANTIENEN** un CONFIG_GET/CONFIG_SET reconciliado — así que conformar = **reconciliar, no borrar** el plano.
  - **`refresh_ai_gate()`**: el seam único de resolve-token-y-setear-gate (análogo a `refresh_architect_ai_runtime`),
    ahora usado por `boot_self_configure`, `handle_vault_secret_changed` **y** CONFIG_SET.
  - **CONFIG_SET (Fork B, owner-confirmado):** era inyector de config; ahora modelo architect — **rechaza TODO**
    campo config/secret/behavior (`frontdesk_rejected_config_field`) y es un **disparador de re-resolve del token
    del vault**; no persiste nada.
  - **CONFIG_GET reconciliado:** `ok` ahora refleja el gate del token (Configured), no la presencia de config;
    `required_fields`/`optional_fields` → `[]`; agrega `config.ai {default_provider, model}` (hive-wide); renombra
    `secrets[]` → `resources[]` (+required) espejando architect; notas autónomas.
  - Removidas las 5 huérfanas de inyección (`ok_response`, `persist_dynamic_config`, `write_json_atomic`,
    `first_secret_bearing_config_field`, `parse_effective_config_doc`; su test re-apuntado). 27/0 tests.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.28` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `0.1.28`; **boot autónomo sigue OK** (degraded → VAULT_SECRET_CHANGED → Configured);
  **regresión handoff determinista** → HTTP 200 `success:true complete`; **0 units failed**. (Nota: la ruta operador
  `GET /nodes/.../config` da `NODE_CONFIG_NOT_FOUND` porque lee un ARCHIVO de config persistido que el nodo autónomo
  ya no tiene — igual que architect; el CONFIG_GET vivo se sirve por el canal mesh, verificado en código + tests.)
- **DIFERIDO (pasada de limpieza catalogada, marcado por el panel):** la ruta `--config`/`run_one_config` (YAML,
  MUERTA en prod — el unit systemd no pasa `--config`) + sus helpers + los tipos de input YAML + los pre-existentes
  `with_jitter`/`parse`.
- **Rollback:** snapshot VM100 `pre-frontdesk-configplane-0-1-28`, o `apt-get install -y --allow-downgrades fluxbee=0.1.27`.

---

## 0.1.27 — frontdesk: bootstrap AUTÓNOMO (resuelve el token del vault al boot + actúa en el broadcast)

- **Fecha:** 2026-08-25 (ART) · **Versión anterior:** 0.1.26 · **Commit:** `000de90` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `SY.frontdesk.gov` (`ai_node_runner`).
- **Qué cambió:** el frontdesk se conforma al patrón canónico de nodo system SY.* (verificado con un panel contra
  `SY.architect` `build_architect_ai_runtime`/`refresh_architect_ai_runtime` y el executor de `SY.admin` — mismo
  mecanismo, mismo seam). Es un nodo **autónomo**: su runtime AI es **baked** (siempre ai_chat — prompt
  `frontdesk_default_instructions` + engine `load_hive_ai_engine` hive.yaml/fallback), y el **único input externo
  es el token del vault**.
  - **Boot:** arma el behavior baked, `boot_self_configure` resuelve el token (`resolve_ai_api_key`, Model D' root)
    → `Configured` si está, sino `Unconfigured` (degradado — el handoff determinista igual anda). Build baked
    falla (hive.yaml roto) → `FAILED_CONFIG`.
  - **Broadcast:** `handle_vault_secret_changed` era un no-op probe+log (EL bug — nunca cambiaba estado); ahora
    **actúa** como `refresh_architect_ai_runtime` — re-resuelve y setea `Configured`/`Unconfigured` en vivo.
  - Reusa solo seams existentes (sin builder ni vault-path nuevos). 27/0 tests.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.27` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `0.1.27` instalado; **el frontdesk pasó a `Configured` desde el token del vault** en el
  arranque (boot degradado 00:52:19 → `VAULT_SECRET_CHANGED` op=put openai_api_key 00:52:20 → **Configured, LLM
  path live**) — sin CONFIG_SET. **Regresión handoff determinista OK** (register_human desde ingress → HTTP 200
  `success:true registration_status:complete`; un 502 transitorio inicial por io.cloud re-registrando su ICH tras
  el restart). frontdesk + io.cloud `active`, `NRestarts=0`, **0 units failed**.
- **DIFERIDO (EDIT 3, owner-confirmado, a pasada de limpieza catalogada):** sacar el plano CONFIG_SET/spawn
  (`apply_config_set` + `load_persisted_dynamic_config` en AMBAS rutas de boot + ~6 helpers) — cascada enredada;
  por la regla "no borrar a ciegas" queda para una pasada dedicada. `apply_config_set` sigue funcional (nadie le
  manda CONFIG_SET al frontdesk en la práctica). Warnings pre-existentes `with_jitter`/`parse` también a esa pasada.
- **Rollback:** snapshot VM100 `pre-frontdesk-autonomous-0-1-27`, o `apt-get install -y --allow-downgrades fluxbee=0.1.26`.

---

## 0.1.26 — frontdesk: el handoff determinista (JSON) corre sin el gate de Configured/LLM (fix register_human)

- **Fecha:** 2026-08-24 (ART) · **Versión anterior:** 0.1.25 · **Commit:** `cb2d192` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `SY.frontdesk.gov` (`ai_node_runner`).
- **Root cause (hallado con la observabilidad de 0.1.25):** `register_human` nunca completaba porque el frontdesk
  **está UNCONFIGURED en todos los boots** (nunca fue seedeado ni recibió CONFIG_SET) y `on_message` rechazaba
  **TODO** mensaje `user` con `node_not_configured` **antes** de mirar el payload → io.cloud lo veía como
  `FRONTDESK_REJECTED` → el ilk quedaba `temporary`. (Confirmado por la línea `Cloud op completed error_code=FRONTDESK_REJECTED` + el frontdesk sin logs de handoff.)
- **Qué cambió:** el alta de humano tiene DOS métodos distinguidos por el **método**, no por ser humano: **auto**
  (llega un JSON `frontdesk_handoff` → determinista, ILK_REGISTER, sin LLM) y **conversacional** (el humano charla
  y el LLM junta los datos). El método determinista se maneja **antes** del gate de Configured → `register_human`
  registra aunque el frontdesk no tenga LLM. El path conversacional/LLM sigue requiriendo Configured.
  `handle_frontdesk_handoff` es config-independiente. Test nuevo `frontdesk_handoff_runs_deterministically_even_when_unconfigured`.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.26` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.26` instalado; **frontdesk reinició (18:40) + `active`**, io.cloud `active`
  `NRestarts=0`, **0 units `failed`**. Pre-deploy: sy-frontdesk-gov 27/0 tests verdes. **Pendiente:** E2E del
  register_human desde la pantalla (debería registrar ahora → ilk `complete`).
- **Tema 2 (separado):** el secret openai NO se perdió; el frontdesk está UNCONFIGURED sólo porque nunca fue
  configurado (peer AI.chat tiene config v2). El path conversacional/LLM necesita configurar el frontdesk — aparte.
- **Rollback:** snapshot VM100 `pre-frontdesk-handoff-0-1-26`, o `apt-get install -y --allow-downgrades fluxbee=0.1.25`.

---

## 0.1.25 — observabilidad: outcome de cada op de Cloud + veredicto del frontdesk (fase 1 consolidación tenant/ILK)

- **Fecha:** 2026-08-24 (ART) · **Versión anterior:** 0.1.24 · **Commit:** `69b1c46` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo runtime `IO.cloud` + el nodo core `SY.frontdesk.gov` (`ai_node_runner`). **Solo logging, cero cambio de comportamiento.**
- **Qué cambió (impacto operativo):** hacer visible el round-trip para diagnosticar por qué un `register_human` no completa (hoy el frontdesk procesa+responde pero no logea nada a INFO → el veredicto es invisible).
  - **io.cloud:** la línea de **egreso/outcome** que faltaba — cada op de Cloud logea `{status, error_code, registration_status, ilk_id, tenant_id, elapsed_ms}` a INFO, con el **mismo `trace_id`** que el ingreso → el round-trip completo edge→io.cloud→admin/identity/frontdesk queda greppable por `trace_id`. (Detalle: dentro de los macros `tracing::*` un `Value` pelado resuelve a `tracing::Value`, así que las lecturas serde usan closures.)
  - **frontdesk (Gov):** promover a INFO/WARN la **decisión del handoff** en cada salida: parseó-como-handoff vs cayó-a-conversacional (**WARN** cuando un payload con forma de handoff NO parsea — la falla silenciosa que estamos cazando), operación no soportada, incompleto→needs_input (ilk NO registrado), REGISTERED (complete), o register FAILED.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.25` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.25` instalado; **io.cloud + sy-frontdesk-gov + sy-admin + sy-identity `active`**, io.cloud `NRestarts=0`, **0 units `failed`**. Pre-deploy: io-cloud 8/0 + sy-frontdesk-gov 26/0 tests verdes.
- **Siguiente:** reproducir `register_human` desde la pantalla de Cloud → los nuevos logs muestran EN VIVO por qué no completa (parseo del handoff o campos faltantes) → fix al source. Después, fase 2 = lecturas (SHM E/!E + `get_ilk_details` relay).
- **Rollback:** snapshot VM100 `pre-observability-0-1-25`, o `apt-get install -y --allow-downgrades fluxbee=0.1.24`.

---

## 0.1.24 — seguridad: reservar los namespaces de infra del vault frente al relay `put_token` de Cloud

- **Fecha:** 2026-08-23 (ART) · **Versión anterior:** 0.1.23 · **Commit:** `35349ef` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `fluxbee_sdk::vault` + el nodo core `SY.admin` (systemd) + el nodo runtime `IO.cloud`.
- **Qué cambió (impacto operativo):** cierra el **MEDIUM** de la auditoría de superficie externa de io.cloud.
  `put_token`→`vault_put` no tenía guarda de namespace de key: un relay de Cloud semi-confiable (o comprometido)
  podía **sobrescribir cualquier key** del vault por charset — incluido `edge_channel_secret:<ich>` (el bearer
  que protege un endpoint externalizado, el suyo propio incluido), `edge_tls`, o `ssh:<hive_id>` (la recovery key
  del spoke). Eso es una superficie de **DoS/takeover**, no "guardar un token de provider".
  - Fix single-source + defense-in-depth: `fluxbee_sdk::vault::CLOUD_RESERVED_VAULT_KEY_PREFIXES`
    (`edge_channel_secret:`, `edge_tls`, `ssh:`) + `is_cloud_reserved_vault_key`. Las keys de peer-auth
    (mesh-HMAC / WAN-mTLS) viven en el **filesystem**, no en el vault, así que ya estaban fuera de alcance.
  - Enforce **server-side autoritativo** en `SY.admin::enforce_cloud_relay_content` (sólo origen `IO.cloud@hive`,
    justo tras `authorize_cloud_relay`, sin bypass) + espejo en `io.cloud::translate_cloud_op` (error temprano
    limpio). Sólo se ata el origen del relay Cloud; los internos SY.* siguen escribiendo esas keys normal.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.24` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.24` instalado; **io.cloud + sy-admin + sy-identity + sy-vault `active`**,
  io.cloud `NRestarts=0`, **0 units `failed`**. Pre-deploy: tests en las 3 capas verdes (SDK vault, io.cloud
  translate, admin enforce) + verificación de wiring (enforce corre siempre tras authorize, sin bypass).
- **Pendiente (no de este deploy):** el diagnóstico del error de `create_tenant` desde Cloud (se ve **mañana con
  el dev** — el alta funciona backend-side; el error está en el round-trip de respuesta cross-hive edge↔io.cloud).
- **Rollback:** snapshot VM100 `pre-vault-guard-0-1-24`, o `apt-get install -y --allow-downgrades fluxbee=0.1.23`.

---

## 0.1.23 — io.cloud: framework de acciones Cloud de primera clase (relay + local) + `register_human` con tenant en la raíz

- **Fecha:** 2026-08-21 (ART) · **Versión anterior:** 0.1.22 · **Commit:** `4abad1b` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca `fluxbee_sdk::cloud` (el vocabulario Cloud compartido) + el nodo runtime `IO.cloud`.
- **Qué cambió (impacto operativo):**
  - `register_human` deja de ser una rama ad-hoc (`op == "register_human"`) y pasa a ser una **acción Cloud
    de primera clase**, despachada por el **set declarado** en el SDK (`CLOUD_LOCAL_OPS`), no por string mágico.
    Dos categorías: **relay** (las 3 de siempre → `SY.admin`) y **local** (`register_human`, `list_cloud_actions`
    → las resuelve io.cloud, **nunca** tocan admin), disjuntas por diseño (un test lo fija — un op local no
    puede filtrarse al gate `authorize_cloud_relay`).
  - **Nueva acción `list_cloud_actions`** (local): devuelve el catálogo de acciones (relay + local) con help,
    para que Fluxbee Cloud **descubra la superficie** sin depender del doc. Cierra el "no discovery API".
  - **Sobre de `register_human` alineado:** el `tenant_id` ahora va en la **raíz** del sobre (como
    put_token/provision_node), ya no en `params`; io.cloud lo inyecta al `frontdesk_handoff` para el frontdesk.
    Es **breaking** vs 0.1.22, pero Cloud aún no tiene nada firme construido y se adapta.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.23` → 245 MB. **Publish:** `apt-repo-publish.sh` → repo :8900.
- **Verificación en vivo:** `fluxbee 0.1.23` instalado; **io.cloud `active/running`, `NRestarts=0`, `ExecMainStatus=0`**
  (conectó al router, ilk propio, ICH `ich:14b66389…` habilitado — sin FAILED_CONFIG); `sy-admin` +
  `sy-orchestrator` + `sy-frontdesk-gov` + los 13 `sy-*` + `rt-gateway` `active/running`; los 9 `fluxbee-node-*`
  `active/running`; **0 units `failed`**. Pre-deploy: SDK cloud tests + workspace io verdes; revisión adversarial
  (2 lentes + verify) → **0 defectos confirmados** (+ 1 hardening de drift del catálogo aplicado).
- **Pendiente:** **E2E funcional** de `register_human` desde Fluxbee Cloud dev (mañana) — no ejecutable en vivo
  desde acá (sin bearer). Contrato final para Cloud en `docs/io-cloud-api.md` §4.4 (`register_human`) / §4.5
  (`list_cloud_actions`).
- **Rollback:** snapshot VM100 `pre-cloud-actions-0-1-23`, o `apt-get install -y --allow-downgrades fluxbee=0.1.22`.

---

## 0.1.22 — frontdesk: fix del bug conversacional + extensión de datos del ilk humano

- **Fecha:** 2026-08-20 (ART) · **Versión anterior:** 0.1.21 · **Commit:** `15fc77f` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). Toca el nodo core `sy-frontdesk-gov` (systemd) + `io_common` (comentario).
- **Qué cambió (impacto operativo):**
  - **BUG arreglado:** el camino conversacional del frontdesk (alcanzable — el force rule 0.1.20 manda el
    mensaje plano de un humano de primer contacto al frontdesk) reportaba `REGISTERED`/`complete`
    **sin haber registrado** cuando el turno del LLM no escribía `thread_state` (turno de charla, o
    registro-y-borrado, ambiguos). Ahora: `None` → `needs_input`/`IN_CONVERSATION` (no falso REGISTERED),
    y el prompt persiste `status=completed` en el éxito (en vez de borrar) para desambiguar. El camino
    determinista (io.cloud `register_human`) **no se toca** (devuelve el resultado del registro directo).
  - **Schema del ilk humano extendido (aditivo, CERO cambio en identity — `identification` es JSONB
    libre guardado verbatim):** `company_name` (typed) + `attributes` (libre) fluyen end-to-end a
    `ILK_REGISTER` por la tool compartida → sirve a los dos caminos. `handle_frontdesk_handoff` antes
    **tiraba** `company_name`; ahora lo reenvía + mergea `attributes` multi-turno (simétrico con company_name).
- **Build:** fb-build (VM110), `build-deb.sh 0.1.22` → 245 MB, 118 entradas.
- **Publish:** `apt-repo-publish.sh` → repo :8900 (22 versiones).
- **Verificación en vivo:** `fluxbee 0.1.22` instalado; **`sy-frontdesk-gov` reiniciado + `running`**
  (02:31 UTC, con el fix+schema); rt-gateway + los 13 `sy-*` `active`; io.cloud `active`; **0 nodos `failed`**.
  Pre-deploy: `sy-frontdesk-gov` tests verdes (26/0, incluye el test que codificaba el bug, corregido) +
  revisión adversarial (bug-fix limpio; 1 LOW de simetría de `attributes` multi-turno, arreglado).
- **Rollback:** snapshot VM100 `pre-frontdesk-0-1-22`, o `apt-get install -y fluxbee=0.1.21`.
- **Pendiente / known:** el camino conversacional depende de que el LLM siga el prompt (misma
  confiabilidad que todo el flujo); un `attributes` libre sin cota de tamaño podría meter blobs grandes
  en el JSONB (validación de tamaño = mejora futura si hace falta). Cierra los dos follow-ups del 0.1.21.

## 0.1.21 — io.cloud `register_human`: registro automático de humanos Cloud→frontdesk

- **Fecha:** 2026-08-20 (ART) · **Versión anterior:** 0.1.20 · **Commit:** `2f60403` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100). io.cloud es un runtime managed motherbee-only; los spokes no lo corren.
- **Qué cambió (impacto operativo):**
  - Nuevo op inbound `register_human` en io.cloud (NO es relay a SY.admin): Fluxbee Cloud manda la data
    del humano como `frontdesk_handoff` JSON; io.cloud **provisiona** un ilk temporary humano (mismo
    `strict_provision_ilk` que io.api) y **Unicastea** el handoff **verbatim** al frontdesk configurado
    (`government.identity_frontdesk`). El frontdesk (path determinista) registra (temporary→complete) y
    responde; io.cloud **relaya el veredicto estructurado** a Cloud, estampado con el `ilk_id` que minteó.
  - **Unicast, no el force rule del router**: un handoff explícito llega al frontdesk con CUALQUIER estado
    del ilk, así que un re-registro de un humano ya `complete` igual aterriza (`ILK_REGISTER` idempotente).
    io.cloud no elige target (usa el frontdesk de config); el force 0.1.20 queda como red de contención de
    emisores implícitos (io.slack/io.wapp).
  - Gate estricto (type + schema_version + operation + tenant `tnt:<uuid>` canónico + email real) y
    `response_envelope` (para que el frontdesk emita el veredicto estructurado, no texto plano).
  - De-diverge: `frontdesk_response_contract` compartido en io_common; io.api de-divergido.
- **Build:** fb-build (VM110), `build-deb.sh 0.1.21` → 245 MB, 118 entradas.
- **Publish:** `apt-repo-publish.sh` → repo :8900 (21 versiones indexadas).
- **Verificación en vivo:** `fluxbee 0.1.21` instalado; runtime **io.cloud movido 0.1.20→0.1.21 + reiniciado
  + sano** (conectado al router como `IO.cloud@motherbee`, self-ilk, ICH habilitado); rt-gateway ruteando;
  13 `sy-*` + rt-gateway `active`; **0 nodos `failed`**. Pre-deploy: io workspace verde (rust 1.92) +
  **doble revisión adversarial** (10 hallazgos 1ra pasada, todos arreglados en la fuente; re-review limpia).
- **Rollback:** snapshot VM100 `pre-register-human-0-1-21`, o `apt-get install -y fluxbee=0.1.20`.
- **Pendiente / known:** (a) **test E2E funcional** de `register_human` (que Cloud lo llame de verdad) —
  el binario corre sano, falta el disparo desde Fluxbee Cloud. (b) 🔖 bug del path conversacional del
  frontdesk (reporta REGISTERED sin registrar). (c) 🔖 `company_name`/`attributes` que el determinista tira
  hoy (extensión del schema del ilk humano). (b)+(c) son el próximo paso.

## 0.1.20 — router: ruteo al frontdesk como política OPA (sin fallback Rust)

- **Fecha:** 2026-08-20 (ART) · **Versión anterior:** 0.1.19 · **Commit:** `1297ba7` (branch `daily_onworking_coa`)
- **Alcance:** **motherbee** (VM100) únicamente. Los spokes (worker1/ingress/egress) siguen en
  **0.1.19** — el `.deb` sólo actualiza motherbee y el core-update a spokes es opcional (misma wasm
  de autoridad ⇒ decisiones idénticas; el ruteo al frontdesk ocurre en motherbee, donde vive la identidad).
- **Qué cambió (impacto operativo):**
  - El ruteo "emisor sin identificar → frontdesk" dejó de ser un `if` hardcodeado en el router
    (`apply_identity_pre_resolve`) y pasó a ser una regla **visible** en `policy/system.rego`
    (nuevo entrypoint `fluxbee/system/frontdesk_route`). Ahora caen al frontdesk **tanto** los ilk
    `temporary` **como** los mensajes **sin ilk** (antes el sin-ilk se perdía en `OpaError::NotLoaded`).
  - Se **eliminó** el fallback Rust `authority()` (duplicaba la política de autoridad SYSTEM):
    **fuente única = el rego**, y el router **falla-cerrado al arrancar** si un `.wasm` horneado no
    carga (`ensure_system_policy_loaded` → `RouterError::Startup`).
  - El re-check `SO-05` del orchestrator ahora usa `authorize_system` (misma política, no un gemelo Rust).
- **Build:** fb-build (VM110), `packaging/build-deb.sh 0.1.20` → `fluxbee_0.1.20_amd64.deb`, 245 MB,
  118 entradas; los `.wasm` van **horneados dentro de `rt-gateway`** (`include_bytes!`), no como archivos.
- **Publish:** `scripts/apt-repo-publish.sh --deb …0.1.20….deb` → `/var/lib/fluxbee-apt` servido en
  `10.10.10.50:8900` (con `-m`, conserva versiones para rollback).
- **Verificación en vivo (post-`apt install`):**
  - `rt-gateway.service` `active (running)` — **arrancó fail-closed OK** (`ensure_system_policy_loaded`
    pasó: los dos wasm horneados cargaron) y **rutea tráfico real** (ADMIN_COMMAND / INVENTORY / VAULT_GET).
  - 13 `sy-*` + `rt-gateway` `running`; **0 nodos `failed`**.
  - Router `/status` **sin user-OPA catchall** ⇒ None→frontdesk es mejora estricta (no redirige tráfico existente).
  - Pre-deploy: tests lib+bins verdes (rust **1.92.0**) + **revisión adversarial** (4 lentes + verificación): **0 defectos confirmados**.
- **Rollback:** snapshot VM100 `pre-router-0-1-20`, o `apt-get install -y fluxbee=0.1.19`.
- **Pendiente / known:** el frontdesk aún **no puede MINTear** un ilk para un emisor totalmente
  sin-ilk (`ILK_PROVISION` es IO-only; `ILK_REGISTER` sólo COMPLETA un temporary existente) — el
  sin-ilk llega al frontdesk pero todavía no se onboardea. Fast-follow separado.

---

<!-- PLANTILLA para la próxima entrada (copiar arriba de esta línea, más reciente primero):

## 0.1.N — <título corto del cambio>

- **Fecha:** YYYY-MM-DD (ART) · **Versión anterior:** 0.1.N-1 · **Commit:** `<sha>` (branch `<rama>`)
- **Alcance:** motherbee | + spokes (worker1/ingress/egress)
- **Qué cambió (impacto operativo):** <1-3 bullets, en términos de qué hace distinto el sistema>
- **Build:** fb-build (VM110), `build-deb.sh 0.1.N` → tamaño / entradas / preflight OK
- **Publish:** `apt-repo-publish.sh` → repo :8900
- **Verificación en vivo:** <servicios `running`, 0 `failed`, chequeo funcional específico del cambio>
- **Rollback:** snapshot `pre-<algo>` · `apt install fluxbee=0.1.N-1`
- **Pendiente / known:** <lo que queda>

-->
