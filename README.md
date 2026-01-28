# json-router

Reinicio del código para alinear con la especificación v1.13.

- Especificación: `docs/01-arquitectura.md` → `docs/08-apendices.md`
- Nodos SY: `docs/legacydocs/SY_nodes_spec.md` (pendiente de actualización a v1.13)

## Sandbox local

Copiar `config/island.yaml` a `/etc/json-router/island.yaml` (o ejecutar el script) y correr el router con:

```sh
sudo ./scripts/install.sh
JSR_ROUTER_NAME=RT.primary cargo run --bin json-router
```

## Nodo de prueba

Ejecutar un nodo de prueba que envía un mensaje `HOLA`:

```sh
cargo run --example node_test
```
Si querés apuntar a un router específico: `JSR_ROUTER_NAME=RT.primary ...`

## Librería de cliente (jsr-client)

La librería para conectar nodos al router vive en `crates/jsr_client`. Es **solo Linux**.

### Instalación paso a paso (proyecto nuevo)

Opción A: copiar el crate dentro de tu repo.

1) Copiá `crates/jsr_client` a tu repo, por ejemplo `vendor/jsr_client`.
2) En tu `Cargo.toml`:

```toml
[dependencies]
jsr-client = { path = "vendor/jsr_client" }
```

Opción B: repo separado (git).

```toml
[dependencies]
jsr-client = { git = "ssh://git@github.com/<org>/jsr-client.git", rev = "<commit>" }
```

### Primitivas mínimas que debe usar tu app

1) Crear `NodeConfig`:
   - `name`: nombre L2 del nodo (ej: `WF.echo`)
   - `router_socket`: socket del router (por UUID o directorio)
   - `uuid_persistence_dir`: donde persistir el UUID del nodo
   - `config_dir`: `/etc/json-router`
   - `version`: versión del nodo (string)

2) Conectar:
   - `NodeClient::connect_with_retry(&config, Duration::from_secs(1))`

3) Enviar:
   - `client.send(&msg).await?`

4) Recibir:
   - `client.recv().await?`

5) Reconectar:
   - Si `recv()` o `send()` falla, reconectar con `connect_with_retry`.

### Ejemplo mínimo

```rust
use jsr_client::{NodeClient, NodeConfig};
use jsr_client::protocol::{Message, Meta, Routing, Destination};
use serde_json::json;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NodeConfig {
        name: "WF.echo".to_string(),
        router_socket: PathBuf::from("/var/run/json-router/routers"),
        uuid_persistence_dir: PathBuf::from("/var/lib/json-router/state/nodes"),
        config_dir: PathBuf::from("/etc/json-router"),
        version: "1.0".to_string(),
    };
    let mut client = NodeClient::connect_with_retry(&config, std::time::Duration::from_secs(1)).await?;

    let msg = Message {
        routing: Routing {
            src: client.uuid().to_string(),
            dst: Destination::Resolve,
            ttl: 1,
            trace_id: uuid::Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            scope: None,
            target: Some("WF.listen@sandbox".to_string()),
            action: None,
            priority: None,
            context: None,
        },
        payload: json!({"type": "text", "content": "HOLA"}),
    };

    client.send(&msg).await?;
    Ok(())
}
```

## Instalación rápida (server de prueba)

Script de instalación base:

```sh
sudo ./scripts/install.sh
```

Rutas fijas:

- `/etc/json-router` (config)
- `/var/lib/json-router/state` (state)
- `/var/run/json-router/routers` (sockets por UUID)

## SY.config.routes

Ejecutar el servicio de config:

```sh
cargo run --bin sy_config_routes
```
Si querés apuntar a un router específico: `JSR_ROUTER_NAME=RT.primary ...`
