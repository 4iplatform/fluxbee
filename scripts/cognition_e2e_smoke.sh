#!/usr/bin/env bash
set -euo pipefail

# End-to-end smoke for SY.cognition without touching AI nodes:
# 1) publish a deterministic turn into storage.turns
# 2) verify raw turns appear in PostgreSQL
# 3) verify cognition_* durable rows appear in PostgreSQL
# 4) verify jsr-memory contains the thread memory_package

CONFIG_FILE="${HIVE_CONFIG:-/etc/fluxbee/hive.yaml}"
SUBJECT="${COGNITION_SUBJECT:-storage.turns}"
TIMEOUT_SECS="${COGNITION_SMOKE_TIMEOUT_SECS:-20}"
KEEP_ROWS="${COGNITION_SMOKE_KEEP_ROWS:-0}"
RUN_SHM_CHECK="${COGNITION_SMOKE_RUN_SHM_CHECK:-1}"

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

sql_value() {
  local query="$1"
  psql "$DB_URL" -tA -c "$query" 2>/dev/null | tr -d '[:space:]'
}

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config file not found: $CONFIG_FILE" >&2
  exit 1
fi

require_cmd python3
require_cmd psql

HIVE_ID="$(awk '/^hive_id:[[:space:]]*/ { sub(/^hive_id:[[:space:]]*/, "", $0); print; exit }' "$CONFIG_FILE" || true)"
HIVE_ID="$(trim_quotes "${HIVE_ID:-sandbox}")"

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

DB_URL="$(python3 - "$DB_URL" <<'PY'
import sys
from urllib.parse import urlsplit, urlunsplit

raw = sys.argv[1].strip()
parts = urlsplit(raw)
path = parts.path or ""
segments = [segment for segment in path.split("/") if segment]
if segments:
    segments[-1] = "fluxbee_storage"
else:
    segments = ["fluxbee_storage"]
normalized = "/" + "/".join(segments)
print(urlunsplit((parts.scheme, parts.netloc, normalized, parts.query, parts.fragment)))
PY
)"

TRACE_ID="cognition-smoke-$(date +%s)-$RANDOM"
THREAD_ID="thread:smoke:${TRACE_ID}"
CTX="trace:${TRACE_ID}"
SRC_ILK="SMOKE.cognition@${HIVE_ID}"
SEQ_ONE=1
SEQ_TWO=2

PAYLOAD_ONE="$(cat <<JSON
{"routing":{"src":"$SRC_ILK","dst":"storage","ttl":16,"trace_id":"$TRACE_ID"},"meta":{"msg_type":"chat","src_ilk":"$SRC_ILK","ich":"smoke://$HIVE_ID","thread_id":"$THREAD_ID","thread_seq":$SEQ_ONE,"ctx":"$CTX","ctx_seq":$SEQ_ONE},"payload":{"type":"text","content":"please help, urgent refund, I was charged twice and this is broken and unacceptable","attachments":[]}}
JSON
)"

PAYLOAD_TWO="$(cat <<JSON
{"routing":{"src":"$SRC_ILK","dst":"storage","ttl":16,"trace_id":"$TRACE_ID-2"},"meta":{"msg_type":"chat","src_ilk":"$SRC_ILK","ich":"smoke://$HIVE_ID","thread_id":"$THREAD_ID","thread_seq":$SEQ_TWO,"ctx":"$CTX","ctx_seq":$SEQ_TWO},"payload":{"type":"text","content":"please resolve this refund issue now, it is still broken and I may escalate the complaint","attachments":[]}}
JSON
)"

python3 - "$NATS_URL" "$SUBJECT" "$PAYLOAD_ONE" "$PAYLOAD_TWO" <<'PY'
import socket
import sys

endpoint = sys.argv[1].strip()
subject = sys.argv[2].strip()
payloads = [arg.encode("utf-8") for arg in sys.argv[3:]]
if endpoint.startswith("nats://"):
    endpoint = endpoint[len("nats://"):]
if ":" not in endpoint:
    endpoint = endpoint + ":4222"
host, port = endpoint.rsplit(":", 1)

s = socket.create_connection((host, int(port)), timeout=3)
s.settimeout(3)
connect = b'CONNECT {"lang":"python","version":"1.0","verbose":false,"pedantic":false,"tls_required":false}\r\n'
cmd = bytearray(connect)
for payload in payloads:
    cmd.extend(b"PUB " + subject.encode("utf-8") + b" " + str(len(payload)).encode("ascii") + b"\r\n" + payload + b"\r\n")
cmd.extend(b"PING\r\n")
s.sendall(cmd)
resp = s.recv(4096).decode("utf-8", "replace")
s.close()
print(resp.strip())
PY

echo "Published 2 cognition smoke turns subject=$SUBJECT thread_id=$THREAD_ID endpoint=$NATS_URL db=$DB_URL"

deadline=$(( $(date +%s) + TIMEOUT_SECS ))
thread_ready=0
turns_ready=0
contexts_ready=0
reasons_ready=0
scopes_ready=0
memories_ready=0
episodes_ready=0

while [[ $(date +%s) -le $deadline ]]; do
  turns_count="$(sql_value "SELECT count(*) FROM turns WHERE ctx = '$CTX';" || echo "0")"
  thread_count="$(sql_value "SELECT count(*) FROM cognition_threads WHERE thread_id = '$THREAD_ID';" || echo "0")"
  context_count="$(sql_value "SELECT count(*) FROM cognition_contexts WHERE thread_id = '$THREAD_ID';" || echo "0")"
  reason_count="$(sql_value "SELECT count(*) FROM cognition_reasons WHERE thread_id = '$THREAD_ID';" || echo "0")"
  scope_count="$(sql_value "SELECT count(*) FROM cognition_scope_instances WHERE thread_id = '$THREAD_ID';" || echo "0")"
  memory_count="$(sql_value "SELECT count(*) FROM cognition_memories WHERE thread_id = '$THREAD_ID';" || echo "0")"
  episode_count="$(sql_value "SELECT count(*) FROM cognition_episodes WHERE thread_id = '$THREAD_ID';" || echo "0")"

  [[ "${turns_count:-0}" =~ ^[0-9]+$ ]] && [[ "$turns_count" -ge 2 ]] && turns_ready=1
  [[ "${thread_count:-0}" =~ ^[0-9]+$ ]] && [[ "$thread_count" -ge 1 ]] && thread_ready=1
  [[ "${context_count:-0}" =~ ^[0-9]+$ ]] && [[ "$context_count" -ge 1 ]] && contexts_ready=1
  [[ "${reason_count:-0}" =~ ^[0-9]+$ ]] && [[ "$reason_count" -ge 1 ]] && reasons_ready=1
  [[ "${scope_count:-0}" =~ ^[0-9]+$ ]] && [[ "$scope_count" -ge 1 ]] && scopes_ready=1
  [[ "${memory_count:-0}" =~ ^[0-9]+$ ]] && [[ "$memory_count" -ge 1 ]] && memories_ready=1
  [[ "${episode_count:-0}" =~ ^[0-9]+$ ]] && [[ "$episode_count" -ge 1 ]] && episodes_ready=1

  if [[ "$turns_ready" == "1" && "$thread_ready" == "1" && "$contexts_ready" == "1" && "$reasons_ready" == "1" && "$scopes_ready" == "1" && "$memories_ready" == "1" && "$episodes_ready" == "1" ]]; then
    break
  fi
  sleep 1
done

if [[ "$turns_ready" != "1" ]]; then
  echo "FAIL: raw turns did not converge into PostgreSQL turns for ctx $CTX within ${TIMEOUT_SECS}s" >&2
  echo "turns=$turns_count thread_id=$THREAD_ID" >&2
  exit 1
fi

echo "OK: raw turns observed in turns"
psql "$DB_URL" -c "SELECT ctx, seq, from_ilk, msg_type FROM turns WHERE ctx = '$CTX' ORDER BY seq;"

if [[ "$thread_ready" != "1" || "$contexts_ready" != "1" || "$reasons_ready" != "1" || "$scopes_ready" != "1" || "$memories_ready" != "1" || "$episodes_ready" != "1" ]]; then
  echo "FAIL: cognition durable rows did not converge for thread $THREAD_ID within ${TIMEOUT_SECS}s" >&2
  echo "turns=$turns_count threads=$thread_count contexts=$context_count reasons=$reason_count scopes=$scope_count memories=$memory_count episodes=$episode_count" >&2
  echo "Hint: query SY.cognition runtime counters with control/config-get and inspect processed_turns_total / published_entities_total / publish_errors_total." >&2
  exit 1
fi

echo "OK: cognition durable rows observed"
psql "$DB_URL" -c "SELECT thread_id, latest_thread_seq, turn_count FROM cognition_threads WHERE thread_id = '$THREAD_ID';"
psql "$DB_URL" -c "SELECT context_id, label, status FROM cognition_contexts WHERE thread_id = '$THREAD_ID' ORDER BY weight DESC LIMIT 5;"
psql "$DB_URL" -c "SELECT reason_id, label, status FROM cognition_reasons WHERE thread_id = '$THREAD_ID' ORDER BY weight DESC LIMIT 5;"
psql "$DB_URL" -c "SELECT scope_instance_id, dominant_context_id, dominant_reason_id FROM cognition_scope_instances WHERE thread_id = '$THREAD_ID' ORDER BY source_ts DESC LIMIT 5;"
psql "$DB_URL" -c "SELECT memory_id, summary FROM cognition_memories WHERE thread_id = '$THREAD_ID' ORDER BY source_ts DESC LIMIT 5;"
psql "$DB_URL" -c "SELECT episode_id, title, intensity FROM cognition_episodes WHERE thread_id = '$THREAD_ID' ORDER BY source_ts DESC LIMIT 5;"

if [[ "$RUN_SHM_CHECK" == "1" ]]; then
  require_cmd cargo
  echo "Checking jsr-memory snapshot for thread $THREAD_ID"
  cargo run --quiet --bin cognition_shm_dump -- --hive "$HIVE_ID" --thread-id "$THREAD_ID"
fi

if [[ "$KEEP_ROWS" == "1" ]]; then
  echo "SKIP cleanup: COGNITION_SMOKE_KEEP_ROWS=1"
  exit 0
fi

psql "$DB_URL" <<SQL
DELETE FROM cognition_episodes WHERE thread_id = '$THREAD_ID';
DELETE FROM cognition_memories WHERE thread_id = '$THREAD_ID';
DELETE FROM cognition_scope_instances WHERE thread_id = '$THREAD_ID';
DELETE FROM cognition_scopes WHERE thread_id = '$THREAD_ID';
DELETE FROM cognition_cooccurrences WHERE thread_id = '$THREAD_ID';
DELETE FROM cognition_reasons WHERE thread_id = '$THREAD_ID';
DELETE FROM cognition_contexts WHERE thread_id = '$THREAD_ID';
DELETE FROM cognition_threads WHERE thread_id = '$THREAD_ID';
DELETE FROM turns WHERE ctx = '$CTX';
SQL

echo "OK: cognition smoke cleanup completed"
