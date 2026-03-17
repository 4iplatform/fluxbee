#!/usr/bin/env bash
set -euo pipefail

# PUB-T25 E2E negative matrix:
# 1) publish must reject config_only package missing runtime_base
# 2) spawn must fail with BASE_RUNTIME_NOT_AVAILABLE for unknown runtime_base
# 3) spawn must fail with BASE_RUNTIME_NOT_PRESENT when base runtime exists in
#    manifest but its start.sh is absent/non-executable on the target hive
#
# This script mutates the local runtime manifest to fabricate the third case in
# a deterministic way, so it currently requires the target hive to be the local
# control-plane hive.
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="motherbee" \
#   MOTHER_HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/runtime_packaging_pub_t25_e2e.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
MOTHER_HIVE_ID="${MOTHER_HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-}"
WAIT_READY_SECS="${WAIT_READY_SECS:-90}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-90}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-90}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
DEPLOY_STRICT="${DEPLOY_STRICT:-1}"
TEST_ID="${TEST_ID:-pubt25-$(date +%s)-$RANDOM}"

MISSING_BASE_RUNTIME_NAME="${MISSING_BASE_RUNTIME_NAME:-wf.publish.neg.missingbase.$(date +%s)}"
MISSING_BASE_RUNTIME_VERSION="${MISSING_BASE_RUNTIME_VERSION:-1.0.0-$TEST_ID}"
UNKNOWN_BASE_RUNTIME_NAME="${UNKNOWN_BASE_RUNTIME_NAME:-wf.publish.neg.unknownbase.$(date +%s)}"
UNKNOWN_BASE_RUNTIME_VERSION="${UNKNOWN_BASE_RUNTIME_VERSION:-2.0.0-$TEST_ID}"
MISSING_START_RUNTIME_NAME="${MISSING_START_RUNTIME_NAME:-wf.publish.neg.basemissingstart.$(date +%s)}"
MISSING_START_RUNTIME_VERSION="${MISSING_START_RUNTIME_VERSION:-3.0.0-$TEST_ID}"
MISSING_START_BASE_RUNTIME_NAME="${MISSING_START_BASE_RUNTIME_NAME:-wf.publish.base.missingstart.$(date +%s)}"
MISSING_START_BASE_RUNTIME_VERSION="${MISSING_START_BASE_RUNTIME_VERSION:-9.0.0-$TEST_ID}"
NODE_UNKNOWN_NAME="${NODE_UNKNOWN_NAME:-WF.publish.neg.unknownbase.$TEST_ID}"
NODE_MISSING_START_NAME="${NODE_MISSING_START_NAME:-WF.publish.neg.basemissingstart.$TEST_ID}"

MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
DIST_MISSING_BASE_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$MISSING_BASE_RUNTIME_NAME"
DIST_UNKNOWN_BASE_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$UNKNOWN_BASE_RUNTIME_NAME"
DIST_MISSING_START_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$MISSING_START_RUNTIME_NAME"
DIST_MISSING_START_BASE_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$MISSING_START_BASE_RUNTIME_NAME"
PUBLISH_BIN="$ROOT_DIR/target/release/fluxbee-publish"

tmpdir="$(mktemp -d)"
missing_pkg_dir="$tmpdir/missing_base_pkg"
unknown_pkg_dir="$tmpdir/unknown_base_pkg"
missing_start_pkg_dir="$tmpdir/missing_start_pkg"
manifest_backup="$tmpdir/manifest.backup.json"
publish_missing_log="$tmpdir/publish_missing.log"
publish_unknown_log="$tmpdir/publish_unknown.log"
publish_missing_start_log="$tmpdir/publish_missing_start.log"
versions_body="$tmpdir/versions.json"
spawn_unknown_body="$tmpdir/spawn_unknown.json"
spawn_missing_start_body="$tmpdir/spawn_missing_start.json"
kill_body="$tmpdir/kill.json"
sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"
meta_file="$tmpdir/meta.txt"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_UNKNOWN_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_MISSING_START_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  if [[ -f "$manifest_backup" ]]; then
    as_root_local install -m 0644 "$manifest_backup" "$MANIFEST_PATH" >/dev/null 2>&1 || true
  fi
  as_root_local rm -rf "$DIST_MISSING_BASE_RUNTIME_DIR" >/dev/null 2>&1 || true
  as_root_local rm -rf "$DIST_UNKNOWN_BASE_RUNTIME_DIR" >/dev/null 2>&1 || true
  as_root_local rm -rf "$DIST_MISSING_START_RUNTIME_DIR" >/dev/null 2>&1 || true
  as_root_local rm -rf "$DIST_MISSING_START_BASE_RUNTIME_DIR" >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
  return "$_ec"
}
trap cleanup EXIT

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  }
}

as_root_local() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    "$@"
  else
    if ! command -v sudo >/dev/null 2>&1; then
      echo "FAIL: sudo is required when not running as root" >&2
      exit 1
    fi
    sudo -n "$@"
  fi
}

http_call() {
  local method="$1"
  local url="$2"
  local out_file="$3"
  local payload="${4:-}"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$out_file" -w "%{http_code}" -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$out_file" -w "%{http_code}" -X "$method" "$url"
  fi
}

json_get_file() {
  local path="$1"
  local file="$2"
  python3 - "$path" "$file" <<'PY'
import json
import sys

path = sys.argv[1]
file_path = sys.argv[2]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

value = doc
for part in path.split("."):
    if not part:
        continue
    if isinstance(value, dict):
        value = value.get(part)
    else:
        value = None
        break

if value is None:
    print("")
elif isinstance(value, bool):
    print("true" if value else "false")
elif isinstance(value, (dict, list)):
    print(json.dumps(value))
else:
    print(str(value))
PY
}

validate_tenant_id() {
  local tenant_id="$1"
  if [[ -z "$tenant_id" ]]; then
    echo "FAIL: TENANT_ID is required (format: tnt:<uuid-v4>)" >&2
    exit 1
  fi
  if [[ ! "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$tenant_id' (expected tnt:<uuid-v4>)" >&2
    exit 1
  fi
}

manifest_version_and_hash() {
  local out_file="$1"
  as_root_local python3 - "$MANIFEST_PATH" "$out_file" <<'PY'
import hashlib
import json
import sys

manifest = sys.argv[1]
out = sys.argv[2]
with open(manifest, "rb") as f:
    raw = f.read()
doc = json.loads(raw.decode("utf-8"))
version = int(doc.get("version", 0) or 0)
sha = hashlib.sha256(raw).hexdigest()
with open(out, "w", encoding="utf-8") as f:
    f.write(f"{version}\n{sha}\n")
PY
}

post_sync_hint() {
  local out_file="$1"
  local payload
  payload="{\"channel\":\"dist\",\"folder_id\":\"fluxbee-dist\",\"wait_for_idle\":true,\"timeout_ms\":$SYNC_HINT_TIMEOUT_MS}"
  http_call "POST" "$BASE/hives/$HIVE_ID/sync-hint" "$out_file" "$payload"
}

wait_sync_hint_ok() {
  local out_file="$1"
  local deadline=$(( $(date +%s) + WAIT_SYNC_HINT_SECS ))
  while (( $(date +%s) <= deadline )); do
    local status
    post_sync_hint "$out_file" >/dev/null
    status="$(json_get_file "payload.status" "$out_file")"
    if [[ "$status" == "ok" ]]; then
      return 0
    fi
    if [[ "$status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: sync-hint status='$status'" >&2
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: sync-hint timeout waiting ok" >&2
  cat "$out_file" >&2 || true
  return 1
}

post_runtime_update() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local out_file="$3"
  local payload
  payload="{\"category\":\"runtime\",\"manifest_version\":${manifest_version},\"manifest_hash\":\"${manifest_hash}\"}"
  http_call "POST" "$BASE/hives/$HIVE_ID/update" "$out_file" "$payload"
}

wait_update_ok_or_sync_pending() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local out_file="$3"
  local deadline=$(( $(date +%s) + WAIT_UPDATE_SECS ))
  while (( $(date +%s) <= deadline )); do
    local status
    post_runtime_update "$manifest_version" "$manifest_hash" "$out_file" >/dev/null
    status="$(json_get_file "payload.status" "$out_file")"
    if [[ "$status" == "ok" || "$status" == "sync_pending" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: update runtime timeout waiting ok|sync_pending" >&2
  cat "$out_file" >&2 || true
  return 1
}

set_manifest_runtime_entry() {
  local runtime="$1"
  local version="$2"
  local package_type="$3"
  local runtime_base="${4:-}"
  as_root_local python3 - "$MANIFEST_PATH" "$runtime" "$version" "$package_type" "$runtime_base" <<'PY'
import json
import sys
import time

manifest_path = sys.argv[1]
runtime = sys.argv[2]
version = sys.argv[3]
package_type = sys.argv[4]
runtime_base = sys.argv[5].strip()

with open(manifest_path, "r", encoding="utf-8") as f:
    doc = json.load(f)

runtimes = doc.get("runtimes")
if not isinstance(runtimes, dict):
    runtimes = {}
doc["runtimes"] = runtimes

entry = runtimes.get(runtime)
if not isinstance(entry, dict):
    entry = {}

available = entry.get("available")
if not isinstance(available, list):
    available = []
available = [str(v).strip() for v in available if str(v).strip()]
available = [v for v in available if v != "current"]
if version not in available:
    available.append(version)

entry["available"] = sorted(set(available))
entry["current"] = version
entry["type"] = package_type
if runtime_base:
    entry["runtime_base"] = runtime_base
else:
    entry.pop("runtime_base", None)

runtimes[runtime] = entry
doc["version"] = int(time.time() * 1000)
doc["updated_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=2, sort_keys=True)
PY
}

wait_runtime_readiness_negative() {
  local runtime_name="$1"
  local runtime_version="$2"
  local expected_manifest_version="${3:-0}"
  local deadline=$(( $(date +%s) + WAIT_READY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http manifest_version runtime_exists readiness_exists present exec base_ready has_base_ready
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    manifest_version="$(jq -r '.payload.hive.runtimes.manifest_version // 0 | tostring' "$versions_body")"
    if [[ "$expected_manifest_version" =~ ^[0-9]+$ && "$expected_manifest_version" -gt 0 ]]; then
      if [[ "$manifest_version" =~ ^[0-9]+$ ]]; then
        if (( manifest_version < expected_manifest_version )); then
          sleep 2
          continue
        fi
      fi
    fi
    runtime_exists="$(jq -r --arg rt "$runtime_name" \
      '(.payload.hive.runtimes.runtimes | has($rt)) // false | tostring' \
      "$versions_body")"
    if [[ "$runtime_exists" != "true" ]]; then
      sleep 2
      continue
    fi
    readiness_exists="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness | has($ver)) // false | tostring' \
      "$versions_body")"
    if [[ "$readiness_exists" != "true" ]]; then
      sleep 2
      continue
    fi
    present="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].runtime_present // false) | tostring' \
      "$versions_body")"
    exec="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].start_sh_executable // false) | tostring' \
      "$versions_body")"
    has_base_ready="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver] | has("base_runtime_ready")) // false | tostring' \
      "$versions_body")"
    base_ready="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].base_runtime_ready // false) | tostring' \
      "$versions_body")"
    if [[ "$has_base_ready" == "true" && "$present" == "true" && "$exec" == "false" && "$base_ready" == "false" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: negative readiness timeout runtime='$runtime_name' version='$runtime_version'" >&2
  cat "$versions_body" >&2 || true
  return 1
}

assert_spawn_error() {
  local expected_http="$1"
  local expected_error_code="$2"
  local response_file="$3"
  local http_value status_value code_value
  http_value="$(cat "$response_file.http")"
  status_value="$(json_get_file "status" "$response_file")"
  code_value="$(json_get_file "error_code" "$response_file")"
  if [[ "$http_value" != "$expected_http" || "$status_value" != "error" || "$code_value" != "$expected_error_code" ]]; then
    echo "FAIL: expected spawn error http=$expected_http code=$expected_error_code, got http=$http_value status=$status_value code=$code_value" >&2
    cat "$response_file" >&2 || true
    return 1
  fi
}

create_missing_base_package_fixture() {
  mkdir -p "$missing_pkg_dir/config" "$missing_pkg_dir/assets"
  cat >"$missing_pkg_dir/package.json" <<EOF
{
  "name": "$MISSING_BASE_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "config_only",
  "description": "PUB-T25 missing runtime_base fixture",
  "config_template": "config/default-config.json"
}
EOF
  cat >"$missing_pkg_dir/config/default-config.json" <<'EOF'
{
  "diag": {
    "kind": "missing_runtime_base"
  }
}
EOF
  cat >"$missing_pkg_dir/assets/readme.txt" <<'EOF'
missing runtime_base fixture
EOF
}

create_unknown_base_package_fixture() {
  mkdir -p "$unknown_pkg_dir/config" "$unknown_pkg_dir/assets"
  cat >"$unknown_pkg_dir/package.json" <<EOF
{
  "name": "$UNKNOWN_BASE_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "config_only",
  "description": "PUB-T25 unknown runtime_base fixture",
  "runtime_base": "wf.publish.base.unknown.$TEST_ID",
  "config_template": "config/default-config.json"
}
EOF
  cat >"$unknown_pkg_dir/config/default-config.json" <<'EOF'
{
  "diag": {
    "kind": "unknown_runtime_base"
  }
}
EOF
  cat >"$unknown_pkg_dir/assets/readme.txt" <<'EOF'
unknown runtime_base fixture
EOF
}

create_missing_start_package_fixture() {
  mkdir -p "$missing_start_pkg_dir/config" "$missing_start_pkg_dir/assets"
  cat >"$missing_start_pkg_dir/package.json" <<EOF
{
  "name": "$MISSING_START_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "config_only",
  "description": "PUB-T25 base runtime missing start.sh fixture",
  "runtime_base": "$MISSING_START_BASE_RUNTIME_NAME",
  "config_template": "config/default-config.json"
}
EOF
  cat >"$missing_start_pkg_dir/config/default-config.json" <<'EOF'
{
  "diag": {
    "kind": "base_runtime_missing_start"
  }
}
EOF
  cat >"$missing_start_pkg_dir/assets/readme.txt" <<'EOF'
base runtime missing start.sh fixture
EOF
}

run_publish_expect_success() {
  local pkg_path="$1"
  local version="$2"
  local out_log="$3"
  as_root_local env \
    FLUXBEE_PUBLISH_BASE="$BASE" \
    FLUXBEE_PUBLISH_MOTHER_HIVE_ID="$MOTHER_HIVE_ID" \
    "$PUBLISH_BIN" "$pkg_path" --version "$version" --deploy "$HIVE_ID" \
    | tee "$out_log"
}

run_publish_expect_failure() {
  local pkg_path="$1"
  local version="$2"
  local out_log="$3"
  set +e
  as_root_local env \
    FLUXBEE_PUBLISH_BASE="$BASE" \
    FLUXBEE_PUBLISH_MOTHER_HIVE_ID="$MOTHER_HIVE_ID" \
    "$PUBLISH_BIN" "$pkg_path" --version "$version" >"$out_log" 2>&1
  local rc=$?
  set -e
  if [[ "$rc" -eq 0 ]]; then
    echo "FAIL: publish unexpectedly succeeded for invalid package '$pkg_path'" >&2
    cat "$out_log" >&2 || true
    exit 1
  fi
}

extract_and_validate_deploy_summary() {
  local out_log="$1"
  local label="$2"
  if ! grep -q "sync_hint_status=" "$out_log"; then
    echo "FAIL[$label]: publish output missing deploy summary" >&2
    cat "$out_log" >&2 || true
    exit 1
  fi
  local deploy_line sync_hint_status update_status update_error_code
  deploy_line="$(grep -E 'sync_hint_status=' "$out_log" | tail -n1)"
  sync_hint_status="$(echo "$deploy_line" | sed -n 's/.*sync_hint_status=\([^ ]*\).*/\1/p')"
  update_status="$(echo "$deploy_line" | sed -n 's/.*update_status=\([^ ]*\).*/\1/p')"
  update_error_code="$(echo "$deploy_line" | sed -n 's/.*update_error_code=\(.*\)$/\1/p')"
  if [[ "$DEPLOY_STRICT" == "1" ]]; then
    if [[ "$sync_hint_status" == "error" ]]; then
      echo "FAIL[$label]: strict deploy requires sync_hint_status != error" >&2
      cat "$out_log" >&2 || true
      exit 1
    fi
    if [[ "$update_status" == "error" ]]; then
      echo "FAIL[$label]: strict deploy requires update_status != error (code='$update_error_code')" >&2
      cat "$out_log" >&2 || true
      exit 1
    fi
  fi
  local manifest_version
  manifest_version="$(sed -n 's/.*Manifest: .* (version=\([0-9][0-9]*\)).*/\1/p' "$out_log" | tail -n1)"
  if [[ -z "$manifest_version" ]]; then
    echo "FAIL[$label]: unable to parse manifest version from publish output" >&2
    cat "$out_log" >&2 || true
    exit 1
  fi
  echo "$sync_hint_status|$update_status|$update_error_code|$manifest_version"
}

spawn_unknown_base() {
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' \
    "$NODE_UNKNOWN_NAME" "$UNKNOWN_BASE_RUNTIME_NAME" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_unknown_body" "$payload" >"$spawn_unknown_body.http"
}

spawn_missing_start() {
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' \
    "$NODE_MISSING_START_NAME" "$MISSING_START_RUNTIME_NAME" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_missing_start_body" "$payload" >"$spawn_missing_start_body.http"
}

require_cmd curl
require_cmd jq
require_cmd python3
require_cmd cargo
validate_tenant_id "$TENANT_ID"

if [[ "$HIVE_ID" != "$MOTHER_HIVE_ID" ]]; then
  echo "FAIL: PUB-T25 currently requires HIVE_ID == MOTHER_HIVE_ID (local manifest/dist mutation for deterministic preflight)." >&2
  echo "HINT: run with HIVE_ID='$MOTHER_HIVE_ID'." >&2
  exit 1
fi

if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "FAIL: runtime manifest missing at '$MANIFEST_PATH'" >&2
  exit 1
fi

echo "PUB-T25 negative E2E: BASE=$BASE HIVE_ID=$HIVE_ID MOTHER_HIVE_ID=$MOTHER_HIVE_ID UNKNOWN_RUNTIME=$UNKNOWN_BASE_RUNTIME_NAME@$UNKNOWN_BASE_RUNTIME_VERSION MISSING_START_RUNTIME=$MISSING_START_RUNTIME_NAME@$MISSING_START_RUNTIME_VERSION"

echo "Step 1/11: build binary (fluxbee-publish)"
(cd "$ROOT_DIR" && cargo build --release --bin fluxbee-publish >/dev/null)
if [[ ! -x "$PUBLISH_BIN" ]]; then
  echo "FAIL: publish binary missing at '$PUBLISH_BIN'" >&2
  exit 1
fi

echo "Step 2/11: backup manifest + create negative package fixtures"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
create_missing_base_package_fixture
create_unknown_base_package_fixture
create_missing_start_package_fixture

echo "Step 3/11: missing runtime_base publish must fail validation"
run_publish_expect_failure "$missing_pkg_dir" "$MISSING_BASE_RUNTIME_VERSION" "$publish_missing_log"
if ! grep -q "runtime_base is required for config_only" "$publish_missing_log"; then
  echo "FAIL: missing runtime_base error not found in publish output" >&2
  cat "$publish_missing_log" >&2 || true
  exit 1
fi

echo "Step 4/11: publish unknown runtime_base package with deploy"
run_publish_expect_success "$unknown_pkg_dir" "$UNKNOWN_BASE_RUNTIME_VERSION" "$publish_unknown_log"
unknown_deploy="$(extract_and_validate_deploy_summary "$publish_unknown_log" "unknown-base")"
unknown_sync_hint_status="${unknown_deploy%%|*}"
unknown_rest="${unknown_deploy#*|}"
unknown_update_status="${unknown_rest%%|*}"
unknown_rest="${unknown_rest#*|}"
unknown_update_error_code="${unknown_rest%%|*}"
unknown_manifest_version="${unknown_rest#*|}"

echo "Step 5/11: unknown runtime_base readiness must stay package-present but base-not-ready"
wait_runtime_readiness_negative "$UNKNOWN_BASE_RUNTIME_NAME" "$UNKNOWN_BASE_RUNTIME_VERSION" "$unknown_manifest_version"

echo "Step 6/11: unknown runtime_base spawn must fail with BASE_RUNTIME_NOT_AVAILABLE"
spawn_unknown_base
assert_spawn_error "500" "BASE_RUNTIME_NOT_AVAILABLE" "$spawn_unknown_body"

echo "Step 7/11: publish dependent package for missing base start.sh case"
run_publish_expect_success "$missing_start_pkg_dir" "$MISSING_START_RUNTIME_VERSION" "$publish_missing_start_log"
missing_start_deploy="$(extract_and_validate_deploy_summary "$publish_missing_start_log" "missing-start")"
missing_start_sync_hint_status="${missing_start_deploy%%|*}"
missing_start_rest="${missing_start_deploy#*|}"
missing_start_update_status="${missing_start_rest%%|*}"
missing_start_rest="${missing_start_rest#*|}"
missing_start_update_error_code="${missing_start_rest%%|*}"

echo "Step 8/11: inject base runtime into manifest without materializing start.sh"
as_root_local rm -rf "$DIST_MISSING_START_BASE_RUNTIME_DIR"
set_manifest_runtime_entry "$MISSING_START_BASE_RUNTIME_NAME" "$MISSING_START_BASE_RUNTIME_VERSION" "full_runtime"
manifest_version_and_hash "$meta_file"
mapfile -t meta < "$meta_file"
missing_start_manifest_version="${meta[0]}"
missing_start_manifest_hash="${meta[1]}"
wait_sync_hint_ok "$sync_hint_body"
wait_update_ok_or_sync_pending "$missing_start_manifest_version" "$missing_start_manifest_hash" "$update_body"

echo "Step 9/11: missing-start readiness must stay package-present but base-not-ready"
wait_runtime_readiness_negative "$MISSING_START_RUNTIME_NAME" "$MISSING_START_RUNTIME_VERSION" "$missing_start_manifest_version"

echo "Step 10/11: spawn must fail with BASE_RUNTIME_NOT_PRESENT"
spawn_missing_start
assert_spawn_error "500" "BASE_RUNTIME_NOT_PRESENT" "$spawn_missing_start_body"

echo "Step 11/11: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "missing_runtime_base_package=$MISSING_BASE_RUNTIME_NAME@$MISSING_BASE_RUNTIME_VERSION"
echo "unknown_runtime_base_package=$UNKNOWN_BASE_RUNTIME_NAME@$UNKNOWN_BASE_RUNTIME_VERSION"
echo "unknown_runtime_base_spawn_error=BASE_RUNTIME_NOT_AVAILABLE"
echo "unknown_sync_hint_status=$unknown_sync_hint_status"
echo "unknown_update_status=$unknown_update_status"
echo "unknown_update_error_code=$unknown_update_error_code"
echo "base_missing_start_package=$MISSING_START_RUNTIME_NAME@$MISSING_START_RUNTIME_VERSION"
echo "base_missing_start_runtime_base=$MISSING_START_BASE_RUNTIME_NAME@$MISSING_START_BASE_RUNTIME_VERSION"
echo "base_missing_start_spawn_error=BASE_RUNTIME_NOT_PRESENT"
echo "missing_start_sync_hint_status=$missing_start_sync_hint_status"
echo "missing_start_update_status=$missing_start_update_status"
echo "missing_start_update_error_code=$missing_start_update_error_code"
echo "runtime packaging PUB-T25 negative E2E passed."
