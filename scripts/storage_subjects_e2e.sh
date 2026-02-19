#!/usr/bin/env bash
set -euo pipefail

# End-to-end smoke for SY.storage subjects:
# 1) publish one message to storage.events
# 2) publish one message to storage.items (linked to event_id)
# 3) publish one message to storage.reactivation (used=true, outcome=resolved)
# 4) verify rows/updates in PostgreSQL events + memory_items
# 5) optional cleanup

CONFIG_FILE="${HIVE_CONFIG:-/etc/fluxbee/hive.yaml}"
TIMEOUT_SECS="${SMOKE_TIMEOUT_SECS:-20}"
KEEP_ROWS="${SMOKE_KEEP_ROWS:-0}"

SUBJECT_EVENTS="storage.events"
SUBJECT_ITEMS="storage.items"
SUBJECT_REACTIVATION="storage.reactivation"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

trim_quotes() {
  local v="$1"
  v="${v#\"}"
  v="${v%\"}"
  printf '%s' "$v"
}

extract_yaml_value() {
  local section="$1"
  local key="$2"
  local file="$3"

  awk -v section="$section" -v key="$key" '
    function ltrim(s) { sub(/^[ \t\r\n]+/, "", s); return s }
    function rtrim(s) { sub(/[ \t\r\n]+$/, "", s); return s }
    function trim(s) { return rtrim(ltrim(s)) }
    {
      line=$0
      if (line ~ /^[^[:space:]][^:]*:[[:space:]]*$/) {
        current=substr(line, 1, length(line)-1)
      }
      if (current == section && line ~ "^[[:space:]]+" key ":[[:space:]]*") {
        sub("^[[:space:]]+" key ":[[:space:]]*", "", line)
        print trim(line)
        exit 0
      }
    }
  ' "$file"
}

publish_nats() {
  local endpoint="$1"
  local subject="$2"
  local payload="$3"
  python3 - "$endpoint" "$subject" "$payload" <<'PY'
import socket
import sys

endpoint = sys.argv[1].strip()
subject = sys.argv[2].strip()
payload = sys.argv[3].encode("utf-8")
if endpoint.startswith("nats://"):
    endpoint = endpoint[len("nats://"):]
if ":" not in endpoint:
    endpoint = endpoint + ":4222"
host, port = endpoint.rsplit(":", 1)

s = socket.create_connection((host, int(port)), timeout=3)
s.settimeout(3)
connect = b'CONNECT {"lang":"python","version":"1.0","verbose":false,"pedantic":false,"tls_required":false}\r\n'
cmd = (
    b"PUB " + subject.encode("utf-8") + b" " +
    str(len(payload)).encode("ascii") + b"\r\n" +
    payload + b"\r\nPING\r\n"
)
s.sendall(connect + cmd)
resp = s.recv(4096).decode("utf-8", "replace")
s.close()
print(resp.strip())
PY
}

wait_sql_true() {
  local label="$1"
  local sql="$2"
  local deadline
  deadline=$(( $(date +%s) + TIMEOUT_SECS ))
  while [[ $(date +%s) -le $deadline ]]; do
    local res
    if ! res="$(psql "$DB_URL" -v ON_ERROR_STOP=1 -tA -c "$sql" 2>&1)"; then
      echo "FAIL: SQL error while waiting '$label'" >&2
      echo "SQL: $sql" >&2
      echo "$res" >&2
      exit 1
    fi
    res="${res//[[:space:]]/}"
    if [[ "$res" == "1" ]]; then
      echo "OK: $label"
      return 0
    fi
    sleep 1
  done
  echo "FAIL: timeout waiting '$label' (${TIMEOUT_SECS}s)" >&2
  echo "SQL: $sql" >&2
  exit 1
}

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config file not found: $CONFIG_FILE" >&2
  exit 1
fi

require_cmd python3
require_cmd psql

NATS_URL="${NATS_URL:-}"
if [[ -z "$NATS_URL" ]]; then
  nats_url_cfg="$(extract_yaml_value "nats" "url" "$CONFIG_FILE" || true)"
  nats_url_cfg="$(trim_quotes "${nats_url_cfg:-}")"
  if [[ -n "$nats_url_cfg" ]]; then
    NATS_URL="$nats_url_cfg"
  else
    nats_port_cfg="$(extract_yaml_value "nats" "port" "$CONFIG_FILE" || true)"
    nats_port_cfg="$(trim_quotes "${nats_port_cfg:-4222}")"
    NATS_URL="nats://127.0.0.1:${nats_port_cfg}"
  fi
fi

DB_URL="${DB_URL:-${FLUXBEE_DATABASE_URL:-}}"
if [[ -z "$DB_URL" ]]; then
  db_url_cfg="$(extract_yaml_value "database" "url" "$CONFIG_FILE" || true)"
  db_url_cfg="$(trim_quotes "${db_url_cfg:-}")"
  DB_URL="$db_url_cfg"
fi

if [[ -z "$DB_URL" ]]; then
  echo "Error: database.url missing (set DB_URL or FLUXBEE_DATABASE_URL)" >&2
  exit 1
fi

TRACE_ID="subjects-$(date +%s)-$RANDOM"
CTX="ctx:${TRACE_ID}"
EVENT_START_SEQ="$(date +%s)"
EVENT_END_SEQ="$((EVENT_START_SEQ + 10))"
MEMORY_ID="mid-${TRACE_ID}"

EVENT_PAYLOAD="$(cat <<JSON
{"event":{"ctx":"$CTX","start_seq":$EVENT_START_SEQ,"end_seq":$EVENT_END_SEQ,"boundary_reason":"smoke_e2e","cues_agg":["smoke","event"],"outcome_status":"open","outcome_duration_ms":1000,"activation_strength":0.5,"context_inhibition":0.0,"use_count":0,"success_count":0}}
JSON
)"

echo "Publishing E2E fixtures trace_id=$TRACE_ID memory_id=$MEMORY_ID endpoint=$NATS_URL"

ack="$(publish_nats "$NATS_URL" "$SUBJECT_EVENTS" "$EVENT_PAYLOAD")"
echo "Published subject=$SUBJECT_EVENTS"
if [[ -n "$ack" ]]; then
  echo "NATS response: $ack"
fi

wait_sql_true \
  "event inserted in events table" \
  "SELECT CASE WHEN EXISTS (SELECT 1 FROM events WHERE ctx = '$CTX' AND start_seq = $EVENT_START_SEQ AND end_seq = $EVENT_END_SEQ) THEN 1 ELSE 0 END;"

EVENT_ID="$(psql "$DB_URL" -v ON_ERROR_STOP=1 -tA -c "SELECT event_id FROM events WHERE ctx = '$CTX' AND start_seq = $EVENT_START_SEQ AND end_seq = $EVENT_END_SEQ ORDER BY event_id DESC LIMIT 1;")"
EVENT_ID="${EVENT_ID//[[:space:]]/}"
if [[ -z "$EVENT_ID" ]]; then
  echo "FAIL: could not resolve event_id after insert (ctx=$CTX)" >&2
  exit 1
fi
echo "Resolved event_id=$EVENT_ID"

ITEM_PAYLOAD="$(cat <<JSON
{"item":{"memory_id":"$MEMORY_ID","event_id":$EVENT_ID,"item_type":"fact","content":{"source":"smoke","trace_id":"$TRACE_ID"},"confidence":0.91,"cues_signature":["smoke","item"],"activation_strength":0.6}}
JSON
)"

REACTIVATION_PAYLOAD="$(cat <<JSON
{"outcome":{"status":"resolved"},"reactivated":{"events":[{"event_id":$EVENT_ID,"used":true}]}}
JSON
)"

ack="$(publish_nats "$NATS_URL" "$SUBJECT_ITEMS" "$ITEM_PAYLOAD")"
echo "Published subject=$SUBJECT_ITEMS"
if [[ -n "$ack" ]]; then
  echo "NATS response: $ack"
fi

wait_sql_true \
  "item inserted in memory_items table" \
  "SELECT CASE WHEN EXISTS (SELECT 1 FROM memory_items WHERE memory_id = '$MEMORY_ID') THEN 1 ELSE 0 END;"

ack="$(publish_nats "$NATS_URL" "$SUBJECT_REACTIVATION" "$REACTIVATION_PAYLOAD")"
echo "Published subject=$SUBJECT_REACTIVATION"
if [[ -n "$ack" ]]; then
  echo "NATS response: $ack"
fi

wait_sql_true \
  "reactivation applied in events table" \
  "SELECT CASE WHEN EXISTS (SELECT 1 FROM events WHERE event_id = $EVENT_ID AND use_count >= 1 AND success_count >= 1 AND activation_strength > 0.5) THEN 1 ELSE 0 END;"

echo "OK: read event row"
psql "$DB_URL" -c "SELECT event_id, ctx, use_count, success_count, activation_strength FROM events WHERE event_id = $EVENT_ID;"

echo "OK: read item row"
psql "$DB_URL" -c "SELECT memory_id, event_id, item_type, confidence FROM memory_items WHERE memory_id = '$MEMORY_ID';"

if [[ "$KEEP_ROWS" == "1" ]]; then
  echo "SKIP cleanup: SMOKE_KEEP_ROWS=1"
  exit 0
fi

deleted_items="$(psql "$DB_URL" -tA -c "WITH d AS (DELETE FROM memory_items WHERE memory_id = '$MEMORY_ID' RETURNING 1) SELECT count(*) FROM d;")"
deleted_items="${deleted_items//[[:space:]]/}"
deleted_events="$(psql "$DB_URL" -tA -c "WITH d AS (DELETE FROM events WHERE event_id = $EVENT_ID RETURNING 1) SELECT count(*) FROM d;")"
deleted_events="${deleted_events//[[:space:]]/}"

if [[ "$deleted_items" != "1" || "$deleted_events" != "1" ]]; then
  echo "FAIL: cleanup removed unexpected rows (items=$deleted_items events=$deleted_events)" >&2
  exit 1
fi

remaining="$(psql "$DB_URL" -tA -c "SELECT (SELECT count(*) FROM events WHERE event_id = $EVENT_ID) + (SELECT count(*) FROM memory_items WHERE memory_id = '$MEMORY_ID');")"
remaining="${remaining//[[:space:]]/}"
if [[ "$remaining" != "0" ]]; then
  echo "FAIL: cleanup verification failed (remaining=$remaining)" >&2
  exit 1
fi

echo "OK: cleanup verified (event_id=$EVENT_ID memory_id=$MEMORY_ID)"
