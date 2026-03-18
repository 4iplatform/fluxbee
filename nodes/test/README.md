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
- cómo adjuntar `src_ilk` en `meta.context.src_ilk`
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
2. `IO.test` envía un mensaje `Resolve` con `meta.context.src_ilk`.
3. El router detecta `registration_status=temporary` y fuerza destino a
   `AI.test.gov@motherbee` sin entrar a OPA.
4. `AI.test.gov` registra el mensaje recibido y devuelve un reply simple a
   `IO.test`.

## Nota importante sobre `src_ilk`

Estos ejemplos escriben `src_ilk` en `meta.context.src_ilk`, porque el router
actual usa ese campo para el pre-resolve de identity-temporary.
