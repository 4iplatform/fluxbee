# WF Generic

Runtime base para nodos `WF.*` instanciables.

Este módulo aloja el runtime genérico del motor de workflows en Go.
El primer corte implementado fija:

- módulo y entrypoint
- tipos de definición del workflow
- carga estricta desde JSON
- validación de load-time
- compilación de guards CEL

El resto del runtime (`store`, `dispatch`, `recover`, `actions`, etc.) se
completa sobre esta base.

## Runbook mínimo

Build local:

```bash
cd ~/fluxbee/go/nodes/wf/wf-generic
go build -o wf-generic .
```

Config de ejemplo usando el workflow canónico:

```bash
cat >/tmp/wf.invoice.config.json <<'EOF'
{
  "workflow_definition_path": "/home/administrator/fluxbee/go/nodes/wf/examples/wf.invoice.json",
  "db_path": "/tmp/wf.invoice.db",
  "sy_timer_l2_name": "SY.timer@motherbee",
  "_system": {
    "node_name": "WF.invoice@motherbee"
  }
}
EOF
```

Run directo:

```bash
cd ~/fluxbee/go/nodes/wf/wf-generic
./wf-generic --config /tmp/wf.invoice.config.json
```

Run estilo managed runtime:

```bash
sudo mkdir -p /var/lib/fluxbee/nodes/WF/WF.invoice@motherbee
sudo cp /tmp/wf.invoice.config.json /var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/config.json
FLUXBEE_NODE_NAME=WF.invoice@motherbee /usr/bin/wf-generic
```

Cliente de ejemplo desde el repo:

```bash
cd ~/fluxbee
cargo run --example wf_client -- start WF.invoice@motherbee cust-001
cargo run --example wf_client -- list WF.invoice@motherbee
cargo run --example wf_client -- get WF.invoice@motherbee <instance_id>
cargo run --example wf_client -- cancel WF.invoice@motherbee <instance_id>
```

## Runbook managed por orchestrator

El modelo correcto de plataforma es:

- `wf.engine` como runtime base
- `wf.invoice` como package `workflow` con `runtime_base = wf.engine`
- `WF.invoice@motherbee` como instancia managed creada por `SY.admin` / `SY.orchestrator`

`wf-generic` soporta ambos caminos:

- config explícita con `workflow_definition_path` para smoke/local dev
- package-native usando `_system.package_path/flow/definition.json` cuando corre como workflow package

Para v1, la config de `WF.*` es boot-time only. No hay `CONFIG_SET/CONFIG_CHANGED` live.

### 1. Publicar el runtime base `wf.engine`

Instalar el binario base:

```bash
cd ~/fluxbee
bash scripts/install.sh
```

Publicar el runtime base `wf.engine` en dist:

```bash
cd ~/fluxbee
bash scripts/publish-wf-runtime.sh --version 0.1.0 --set-current --sudo
```

### 2. Publicar el package workflow `wf.invoice`

```bash
cd ~/fluxbee
bash scripts/publish-wf-invoice-package.sh --version 0.1.0 --deploy motherbee
```

### 3. Spawn managed vía `SY.admin`/`SY.orchestrator`

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes" \
  -H "Content-Type: application/json" \
  -d '{
    "node_name": "WF.invoice@motherbee",
    "runtime": "wf.invoice",
    "runtime_version": "current",
    "tenant_id": "tnt:REEMPLAZAR",
    "config": {
      "tenant_id": "tnt:REEMPLAZAR"
    }
  }' | jq
```

Notas:

- `workflow_definition_path` ya no hace falta; el runtime la resuelve desde `_system.package_path/flow/definition.json`
- `db_path` por default queda en el instance dir: `wf_instances.db`
- `sy_timer_l2_name` por default se deriva como `SY.timer@<hive>`

### 4. Verificar estado

```bash
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/status" | jq
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/config" | jq
```

### 5. Probar flujo con el cliente de ejemplo

```bash
cd ~/fluxbee
cargo run --example wf_client -- start WF.invoice@motherbee cust-001
cargo run --example wf_client -- list WF.invoice@motherbee
cargo run --example wf_client -- get WF.invoice@motherbee <instance_id>
cargo run --example wf_client -- cancel WF.invoice@motherbee <instance_id>
```
