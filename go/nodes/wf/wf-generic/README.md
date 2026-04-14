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
