```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="AI.support.rep@motherbee"
TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
PROMPT_FILE="/tmp/ai_support_rep.prompt.txt"
CONFIG_JSON="/tmp/AI.support.rep.config.json"
OPENAI_API_KEY="sk-proj-xxx"

mkdir -p "$(dirname "$CONFIG_JSON")"
: > "$CONFIG_JSON"

PROMPT_FILE="$PROMPT_FILE" \
CONFIG_JSON="$CONFIG_JSON" \
TENANT_ID="$TENANT_ID" \
OPENAI_API_KEY="$OPENAI_API_KEY" \
python3 - <<'PY'
import json
import os
from pathlib import Path

prompt = Path(os.environ["PROMPT_FILE"]).read_text(encoding="utf-8")
doc = {
    "tenant_id": os.environ["TENANT_ID"],
    "behavior": {
        "kind": "openai_chat",
        "model": "gpt-4.1-mini",
        "instructions": {
            "source": "inline",
            "value": prompt,
            "trim": True,
        },
        "model_settings": {
            "temperature": 0.2,
            "top_p": 1.0,
            "max_output_tokens": 600,
        },
    },
    "runtime": {
        "handler_timeout_ms": 120000,
        "worker_pool_size": 4,
        "queue_capacity": 128,
        "immediate_memory": {
            "enabled": True,
            "recent_interactions_max": 10,
            "active_operations_max": 8,
            "summary_max_chars": 1600,
            "summary_refresh_every_turns": 3,
            "trim_noise_enabled": True,
        },
    },
    "secrets": {
        "openai": {
            "api_key": os.environ["OPENAI_API_KEY"],
        }
    },
}

Path(os.environ["CONFIG_JSON"]).write_text(
    json.dumps(doc, ensure_ascii=False, indent=2) + "\n",
    encoding="utf-8",
)
PY

jq . "$CONFIG_JSON"
```
