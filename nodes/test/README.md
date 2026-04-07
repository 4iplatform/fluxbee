# Test Nodes Workspace

Este directorio contiene nodos de ejemplo orientados a validar transporte/routing
real con el SDK, pero escritos para que también sirvan como referencia a equipos
externos.

Incluye dos runtimes mí­nimos:

- `IO.test`: conecta al router, obtiene/provisiona un ILK temporal opcionalmente
  y enví­a un mensaje `Resolve`.
- `AI.test.gov`: actúa como frontdesk simple, recibe el mensaje ruteado por el
  router y responde con un ack.
- `IO.test.cognition`: emisor disposable para E2E de cognition por router con
  `thread_id` canónico.
- `AI.test.cognition`: receptor disposable que captura
  `trace_id/ich/thread_id/thread_seq/context/memory_package` y responde con observaciones.

### E2E cognition por router: qué valida hoy

El par `IO.test.cognition` + `AI.test.cognition` valida el carrier real por
router, no un publish lateral a NATS. El chequeo actual cubre:

- preservación de `trace_id`
- preservación de `ich`
- entrega al receptor esperado
- `msg_type=user`
- preservación de `thread_id`
- asignación monotónica de `thread_seq`
- presencia de `meta.context.probe_id/probe_step`
- enrichment posterior con `memory_package`

## Objetivo

Mostrar, con nodos reales y legibles:

- cómo construir `NodeConfig` y conectar con `fluxbee_sdk::connect`
- cómo emitir un mensaje `Resolve`
- cómo adjuntar `src_ilk` en `meta.src_ilk`
- cómo responder `NODE_STATUS_GET` con el handler default del SDK
- cómo observar el auto-routing hacia el frontdesk configurado en `hive.yaml`

## Estructura

```text
nodes/test/
├── ai-test-gov/
├── ai-test-cognition/
├── io-test/
└── io-test-cognition/
```

## Build

Desde raí­z del repo:

```bash
cargo check -p io-test
cargo check -p ai-test-gov
cargo check -p io-test-cognition
cargo check -p ai-test-cognition
```

## Probar auto-routing de ILK temporal

En `/etc/fluxbee/hive.yaml` del hive local:

```yaml
government:
  identity_frontdesk: "AI.test.gov@motherbee"
```

Reiniciar el router si cambió esa config.

Terminal 1:

```bash
cargo run -p ai-test-gov
```

Terminal 2:

```bash
IO_TEST_ALLOW_PROVISION=1 \
IO_TEST_IDENTITY_TARGET="SY.identity@motherbee" \
cargo run -p io-test
```

Comportamiento esperado:

1. `IO.test` provisiona un ILK temporal si no recibe uno por env.
2. `IO.test` enví­a un mensaje `Resolve` con `meta.src_ilk`.
3. El router detecta `registration_status=temporary` y fuerza destino a
   `AI.test.gov@motherbee` sin entrar a OPA.
4. `AI.test.gov` registra el mensaje recibido y devuelve un reply simple a
   `IO.test`.

## Nota importante sobre `src_ilk`

El contrato canónico ahora es `meta.src_ilk`.

## Contrato de nombre para nodos gestionados

Para nodos gestionados spawneados por `SY.orchestrator`, el contrato de bootstrap
de nombre para v1 es:

- `SY.orchestrator` inyecta `FLUXBEE_NODE_NAME=<node_name@hive>` al proceso
- el runtime usa ese nombre como identidad canónica al arrancar
- el `config.json` de la instancia se deriva desde ese nombre con el layout:
  - `/var/lib/fluxbee/nodes/<KIND>/<node_name@hive>/config.json`

Esto aplica tanto al spawn normal como al relaunch de reboot/reconcile.

Ejemplo:

```bash
FLUXBEE_NODE_NAME="SY.frontdesk.gov@motherbee"
```

Para ese caso, el path de instancia esperado es:

```text
/var/lib/fluxbee/nodes/SY/SY.frontdesk.gov@motherbee/config.json
```

### Por qué este contrato

- evita duplicar `NODE_NAME` y `NODE_CONFIG` como dos referencias que podrí­an divergir
- evita depender de argumentos en todos los `start.sh`
- concentra el cambio en `SY.orchestrator` usando `systemd-run --setenv=...`
- sirve igual para singleton e instanciado

### Helper del SDK

El SDK ya trae helpers para este contrato:

- `fluxbee_sdk::managed_node_name(...)`
  - toma `FLUXBEE_NODE_NAME` como fuente principal
  - puede caer a env vars legacy del runtime si hace falta compatibilidad
- `fluxbee_sdk::managed_node_config_path(...)`
  - deriva el `config.json` canónico a partir del `node_name`

Patrón recomendado en un runtime:

```rust
let cfg = NodeConfig {
    name: fluxbee_sdk::managed_node_name("AI.test.gov", &["AI_TEST_NODE_NAME"]),
    router_socket: "/var/run/fluxbee/routers".into(),
    uuid_persistence_dir: "/var/lib/fluxbee/state/nodes".into(),
    uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
    config_dir: "/etc/fluxbee".into(),
    version: "0.1.0".to_string(),
};
```

Notas prácticas:

- si el proceso arranca bajo orchestrator, `FLUXBEE_NODE_NAME` deberí­a ganar siempre
- las env vars legacy (`AI_TEST_NODE_NAME`, `IO_TEST_NODE_NAME`, `GOV_NODE_NAME`) quedan sólo como compatibilidad o uso manual
- `_system.node_name` dentro de `config.json` sigue siendo la fuente de verdad persistida

## Nota sobre prefijos de nodo y lifecycle

Los prefijos funcionales del nombre (`AI.*`, `IO.*`, `WF.*`, `SY.*`, `RT.*`) no
son lo mismo que la clasificación de lifecycle (`core` vs runtime gestionado).

Ejemplos:

- `AI.*`, `IO.*` y `WF.*` suelen correr como runtimes gestionados publicados
  en `dist`
- un nodo `AI.*` puede cumplir un rol muy importante en la arquitectura y aun así­
  seguir estando en el modelo de runtime gestionado
- `SY.*` identifica control-plane/core del sistema
- `RT.gateway` identifica infraestructura base del hive

Entonces, para v1, la regla operativa no es "AI/IO/WF sí­, resto no".

La regla real es:

- `SY.*` no entra por spawn gestionado
- `RT.gateway` no entra por spawn gestionado
- `AI.*`, `IO.*`, `WF.*` y `RT.<otro>` pueden entrar por el modelo de runtime gestionado publicado

Eso evita mezclar:

- familia funcional del nodo
- origen/lifecycle del componente

## Probar publish/install completo con CLI

También hay un E2E operator-style que usa `fluxbee-publish` de punta a punta
con estos dos nodos:

```bash
BASE="http://127.0.0.1:8080" \
HIVE_ID="motherbee" \
MOTHER_HIVE_ID="motherbee" \
TENANT_ID="tnt:<uuid-v4>" \
bash scripts/identity_test_nodes_publish_e2e.sh
```

Ese script:

1. compila `fluxbee-publish`, `ai-test-gov` e `io-test`
2. arma packages `full_runtime` temporales
3. publica ambos con `--deploy`
4. espera readiness en `/versions`
5. spawnea `AI.test.gov` como frontdesk gestionado
6. spawnea `IO.test` como probe one-shot
7. valida que el reply vuelva con `HANDLED_BY=AI.test.gov@<hive>`

Nota:

- este E2E requiere `TENANT_ID`, porque el frontdesk configurado en `hive.yaml`
  se spawnea como nodo gestionado y el orchestrator hoy exige `tenant_id` para
  ese camino de registro identity-aware

Sirve para verificar el ciclo completo de software:

- package layout
- install en `dist`
- update/sync
- spawn administrado
- ejecución real del binario publicado

También quedó validado un E2E para reboot/reconcile de nodos custom gestionados:

```bash
BASE="http://127.0.0.1:8080" \
HIVE_ID="motherbee" \
MOTHER_HIVE_ID="motherbee" \
TENANT_ID="tnt:<uuid-v4>" \
bash scripts/custom_node_reboot_reconcile_e2e.sh
```

Ese script valida:

1. publish de runtimes temporales
2. spawn del frontdesk gestionado usando por default el `government.identity_frontdesk` configurado en `hive.yaml`
3. `kill_node`
4. restart de `sy-orchestrator`
5. relaunch automático desde `config.json`
6. probe real posterior con `IO.test`

## E2E cognition por router

Estos nodos existen para validar cognition v2 sin tocar AI nodes productivos y
sin bypass por `storage.turns`.

Terminal 1:

```bash
cargo run -p ai-test-cognition
```

Terminal 2:

```bash
cargo run -p io-test-cognition
```

Comportamiento esperado:

1. `IO.test.cognition` calcula un `thread_id` canónico y enví­a un primer turn
   unicast a `AI.test.cognition@<hive>`.
2. El router asigna `thread_seq` y entrega al nodo receptor.
3. `AI.test.cognition` devuelve en el reply su observación del mensaje recibido:
   - `thread_id`
   - `thread_seq`
   - presencia/ausencia de `memory_package`
4. `IO.test.cognition` enví­a turns adicionales sobre el mismo thread hasta
   observar `memory_package` o agotar el máximo configurado.

Variables útiles:

- `IO_TEST_COGNITION_TARGET`
- `IO_TEST_COGNITION_PROBE_ID`
- `IO_TEST_COGNITION_THREAD_ID`
- `IO_TEST_COGNITION_MAX_STEPS`
- `IO_TEST_COGNITION_TURN_DELAY_MS`
- `IO_TEST_COGNITION_REQUIRE_MEMORY_PACKAGE`
- `AI_TEST_COGNITION_CAPTURE_PATH`

Salida útil del emisor:

- `THREAD_ID`
- `FIRST_THREAD_SEQ`
- `FINAL_THREAD_SEQ`
- `MEMORY_PACKAGE_PRESENT`
- `DOMINANT_CONTEXT`
- `DOMINANT_REASON`
- `FINAL_OBSERVATION`
