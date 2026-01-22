# json-router

Reinicio del código para alinear con la especificación v1.13.

- Especificación: `docs/01-arquitectura.md` → `docs/08-apendices.md`
- Nodos SY: `docs/legacydocs/SY_nodes_spec.md` (pendiente de actualización a v1.13)

## Sandbox local

Copiar `config/island.yaml` a `/etc/json-router/island.yaml` (o ejecutar el script) y correr el router con:

```sh
./scripts/install_sandbox.sh
JSR_ROUTER_NAME=RT.primary cargo run --bin json-router
```

## Nodo de prueba

Ejecutar un nodo de prueba que envía un mensaje `HOLA`:

```sh
JSR_ROUTER_NAME=RT.primary cargo run --example node_test
```

## Instalación rápida (server de prueba)

Script de instalación base:

```sh
./scripts/install_sandbox.sh
```

Rutas fijas:

- `/etc/json-router` (config)
- `/var/lib/json-router/state` (state)
- `/var/run/json-router/routers` (sockets por UUID)

## SY.config.routes

Ejecutar el servicio de config:

```sh
JSR_ROUTER_NAME=RT.primary cargo run --bin sy_config_routes
```
