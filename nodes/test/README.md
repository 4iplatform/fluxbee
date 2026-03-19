# Test Nodes Workspace

Este directorio contiene nodos de ejemplo orientados a validar transporte/routing
real con el SDK, pero escritos para que también sirvan como referencia a equipos
externos.

Incluye dos runtimes mínimos:

- `IO.test`: conecta al router, obtiene/provisiona un ILK temporal opcionalmente
  y envía un mensaje `Resolve`.
- `AI.test.gov`: actúa como frontdesk simple, recibe el mensaje ruteado por el
  router y responde con un ack.

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
└── io-test/
```

## Build

Desde raíz del repo:

```bash
cargo check -p io-test
cargo check -p ai-test-gov
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
2. `IO.test` envía un mensaje `Resolve` con `meta.src_ilk`.
3. El router detecta `registration_status=temporary` y fuerza destino a
   `AI.test.gov@motherbee` sin entrar a OPA.
4. `AI.test.gov` registra el mensaje recibido y devuelve un reply simple a
   `IO.test`.

## Nota importante sobre `src_ilk`

El contrato canónico ahora es `meta.src_ilk`.

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

También quedó preparado un E2E para reboot/reconcile de nodos custom gestionados:

```bash
BASE="http://127.0.0.1:8080" \
HIVE_ID="motherbee" \
MOTHER_HIVE_ID="motherbee" \
TENANT_ID="tnt:<uuid-v4>" \
bash scripts/custom_node_reboot_reconcile_e2e.sh
```

Ese script valida:

1. publish de runtimes temporales
2. spawn del frontdesk gestionado
3. `kill_node`
4. restart de `sy-orchestrator`
5. relaunch automático desde `config.json`
6. probe real posterior con `IO.test`
