spawn
```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "0.1.3" \
  --node-name "$NODE_NAME" \
  --tenant-id "$TENANT_ID" \
  --config-json "$CONFIG_JSON" \
  --spawn \
  --sync-hint \
  --sudo
```
validate
```bash
curl -sS "$BASE/hives/$HIVE_ID/nodes" | jq '.payload.nodes[] | select(.node_name=="AI.support.rep@motherbee")'
```
logs
```bash
sudo journalctl -f | rg "AI.support.rep|fluxbee-node-AI.support.rep-motherbee|rt-gateway|HELLO|ANNOUNCE|node registered|node disconnected"
```