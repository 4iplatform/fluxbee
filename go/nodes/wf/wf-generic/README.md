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

1. Instalar el binario base:

```bash
cd ~/fluxbee
bash scripts/install.sh
```

2. Publicar el runtime base `wf.engine` en dist:

```bash
cd ~/fluxbee
bash scripts/publish-wf-runtime.sh --version 0.1.0 --set-current --sudo
```

3. Materializar la definición del workflow en el instance dir:

```bash
sudo mkdir -p /var/lib/fluxbee/nodes/WF/WF.invoice@motherbee
sudo cp /home/administrator/fluxbee/go/nodes/wf/examples/wf.invoice.json \
  /var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/wf.invoice.json
```

4. Spawn managed vía `SY.admin`/`SY.orchestrator`:

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes" \
  -H "Content-Type: application/json" \
  -d '{
    "node_name": "WF.invoice@motherbee",
    "runtime": "wf.engine",
    "runtime_version": "current",
    "config": {
      "workflow_definition_path": "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/wf.invoice.json",
      "db_path": "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/wf_instances.db",
      "sy_timer_l2_name": "SY.timer@motherbee"
    }
  }' | jq
```

5. Verificar estado:

```bash
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/status" | jq
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/config" | jq
```

6. Probar flujo con el cliente de ejemplo:

```bash
cd ~/fluxbee
cargo run --example wf_client -- start WF.invoice@motherbee cust-001
cargo run --example wf_client -- list WF.invoice@motherbee
cargo run --example wf_client -- get WF.invoice@motherbee <instance_id>
cargo run --example wf_client -- cancel WF.invoice@motherbee <instance_id>
```
