# json-router

Reinicio del código para alinear con la especificación v1.13.

- Especificación: `docs/01-arquitectura.md` → `docs/08-apendices.md`
- Nodos SY: `docs/legacydocs/SY_nodes_spec.md` (pendiente de actualización a v1.13)

## Sandbox local

Crear `config/island.yaml` (ya incluido en este repo) y correr el router con:

```sh
JSR_ROUTER_NAME=RT.primary \\
JSR_CONFIG_DIR=./config \\
JSR_STATE_DIR=./state \\
JSR_SOCKET_DIR=./run \\
cargo run
```

## Nodo de prueba

Ejecutar un nodo de prueba que envía un mensaje `HOLA`:

```sh
JSR_CONFIG_DIR=/etc/json-router \
JSR_SOCKET_DIR=/var/run/json-router \
JSR_NODE_UUID_DIR=/var/lib/json-router/nodes \
cargo run --example node_test
```

## Instalación rápida (server de prueba)

Script de instalación base:

```sh
./scripts/install_sandbox.sh
```

Variables opcionales:

- `JSR_CONFIG_DIR` (default `/etc/json-router`)
- `JSR_STATE_DIR` (default `/var/lib/json-router`)
- `JSR_SOCKET_DIR` (default `/var/run/json-router`)
- `JSR_ISLAND_ID` (default `sandbox`)
- `JSR_ROUTER_NAME` (default `RT.primary`)

## SY.config.routes

Ejecutar el servicio de config:

```sh
JSR_CONFIG_DIR=/etc/json-router \
JSR_STATE_DIR=/var/lib/json-router \
JSR_SOCKET_DIR=/var/run/json-router \
cargo run --bin sy_config_routes
```
