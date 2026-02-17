#!/usr/bin/env bash
set -euo pipefail

# End-to-end smoke for SY.storage ingestion:
# 1) publish one message to NATS subject storage.turns
# 2) verify row is inserted into PostgreSQL turns table
# 3) verify row can be read back from PostgreSQL
# 4) delete row and verify cleanup

CONFIG_FILE="${HIVE_CONFIG:-/etc/fluxbee/hive.yaml}"
SUBJECT="${STORAGE_SUBJECT:-storage.turns}"
TIMEOUT_SECS="${SMOKE_TIMEOUT_SECS:-15}"
KEEP_ROW="${SMOKE_KEEP_ROW:-0}"

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

TRACE_ID="smoke-$(date +%s)-$RANDOM"
CTX="trace:${TRACE_ID}"
SEQ="$(date +%s)"
FROM_ILK="SMOKE.publisher@${HIVE_ID}"

PAYLOAD_JSON="$(cat <<JSON
{"routing":{"src":"$FROM_ILK","dst":"storage","ttl":16,"trace_id":"$TRACE_ID"},"meta":{"ctx":"$CTX","ctx_seq":$SEQ,"src_ilk":"$FROM_ILK","msg_type":"test"},"payload":{"message":"smoke-storage-ingest","tags":["smoke","storage"]}}
JSON
)"

NATS_ACK="$(python3 - "$NATS_URL" "$SUBJECT" "$PAYLOAD_JSON" <<'PY'
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
cmd = b"PUB " + subject.encode("utf-8") + b" " + str(len(payload)).encode("ascii") + b"\r\n" + payload + b"\r\nPING\r\n"
s.sendall(connect + cmd)
resp = s.recv(4096).decode("utf-8", "replace")
s.close()
print(resp.strip())
PY
)"

echo "Published subject=$SUBJECT trace_id=$TRACE_ID endpoint=$NATS_URL"
if [[ -n "$NATS_ACK" ]]; then
  echo "NATS response: $NATS_ACK"
fi

deadline=$(( $(date +%s) + TIMEOUT_SECS ))
inserted=0

while [[ $(date +%s) -le $deadline ]]; do
  count="$(psql "$DB_URL" -tA -c "SELECT count(*) FROM turns WHERE ctx = '$CTX' AND seq = $SEQ;" 2>/dev/null || echo "0")"
  count="${count//[[:space:]]/}"
  if [[ "$count" =~ ^[0-9]+$ ]] && [[ "$count" -ge 1 ]]; then
    inserted=1
    break
  fi
  sleep 1
done

if [[ "$inserted" -ne 1 ]]; then
  echo "FAIL: no insert observed in turns (ctx=$CTX seq=$SEQ) within ${TIMEOUT_SECS}s" >&2
  exit 1
fi

echo "OK: insert observed in turns (ctx=$CTX seq=$SEQ)"

row="$(psql "$DB_URL" -tA -F '|' -c "SELECT ctx, seq, from_ilk, msg_type FROM turns WHERE ctx = '$CTX' AND seq = $SEQ LIMIT 1;")"
if [[ -z "${row//[[:space:]]/}" ]]; then
  echo "FAIL: inserted row not readable from turns (ctx=$CTX seq=$SEQ)" >&2
  exit 1
fi

echo "OK: read observed in turns"
psql "$DB_URL" -c "SELECT ctx, seq, from_ilk, msg_type FROM turns WHERE ctx = '$CTX' AND seq = $SEQ;"

if [[ "$KEEP_ROW" == "1" ]]; then
  echo "SKIP cleanup: SMOKE_KEEP_ROW=1"
  exit 0
fi

deleted="$(psql "$DB_URL" -tA -c "WITH d AS (DELETE FROM turns WHERE ctx = '$CTX' AND seq = $SEQ RETURNING 1) SELECT count(*) FROM d;")"
deleted="${deleted//[[:space:]]/}"
if [[ "$deleted" != "1" ]]; then
  echo "FAIL: delete step did not remove exactly one row (deleted=$deleted)" >&2
  exit 1
fi

remaining="$(psql "$DB_URL" -tA -c "SELECT count(*) FROM turns WHERE ctx = '$CTX' AND seq = $SEQ;")"
remaining="${remaining//[[:space:]]/}"
if [[ "$remaining" != "0" ]]; then
  echo "FAIL: cleanup verification failed (remaining=$remaining)" >&2
  exit 1
fi

echo "OK: delete observed and verified (ctx=$CTX seq=$SEQ)"
