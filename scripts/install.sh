#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="/etc/fluxbee"
STATE_DIR="/var/lib/fluxbee"
RUN_DIR="/var/run/fluxbee"
APPLY_DEV_OWNERSHIP="${APPLY_DEV_OWNERSHIP:-1}"
INSTALL_OWNER="${INSTALL_OWNER:-${SUDO_USER:-$USER}}"
RESTART_ORCHESTRATOR_AFTER_INSTALL="${RESTART_ORCHESTRATOR_AFTER_INSTALL:-1}"
SEED_RUNTIME_FIXTURE="${SEED_RUNTIME_FIXTURE:-1}"
RUNTIME_FIXTURE_NAME="${RUNTIME_FIXTURE_NAME:-wf.orch.diag}"
RUNTIME_FIXTURE_VERSION="${RUNTIME_FIXTURE_VERSION:-0.0.1}"
RUNTIME_FIXTURE_SLEEP_SECS="${RUNTIME_FIXTURE_SLEEP_SECS:-3600}"
BIN_DIR="${BIN_DIR:-$ROOT_DIR/target/release}"

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    # Common case: running with plain sudo loses user PATH (cargo unavailable as root).
    # If release binaries already exist, continue without rebuilding.
    if [[ -x "$BIN_DIR/json-router" && -x "$BIN_DIR/sy_orchestrator" && -x "$BIN_DIR/sy_identity" && -x "$BIN_DIR/sy_cognition" ]]; then
      echo "Warning: cargo not found; using prebuilt binaries from $BIN_DIR (SKIP_BUILD=1)."
      SKIP_BUILD=1
    else
      echo "Error: cargo not found and required prebuilt binaries are missing in $BIN_DIR." >&2
      echo "Hint: run without sudo (the script already uses sudo internally), or install cargo in root PATH, or set SKIP_BUILD=1 with existing binaries." >&2
      exit 1
    fi
  fi

  if [[ "${SKIP_BUILD:-}" != "1" ]] && ! command -v protoc >/dev/null 2>&1; then
    echo "Error: protoc not found in PATH." >&2
    echo "Fluxbee now builds LanceDB-backed components and requires the Protocol Buffers compiler on the build host." >&2
    echo "Debian/Ubuntu: sudo apt-get update && sudo apt-get install -y protobuf-compiler" >&2
    echo "RHEL/CentOS: sudo dnf install -y protobuf-compiler" >&2
    exit 1
  fi

  if [[ "${SKIP_BUILD:-}" != "1" ]]; then
    echo "Building Rust binaries..."
    cargo build --release --bins
    echo "Building sy-frontdesk-gov core binary..."
    cargo build --release -p sy-frontdesk-gov --bin ai_node_runner
    cp "$BIN_DIR/ai_node_runner" "$BIN_DIR/sy-frontdesk-gov"
  fi
fi

if [[ -d "$ROOT_DIR/sy-opa-rules" ]]; then
  if [[ "${SKIP_BUILD:-}" == "1" || "${SKIP_GO_BUILD:-}" == "1" ]]; then
    echo "SKIP_BUILD/SKIP_GO_BUILD set; skipping sy-opa-rules build."
  elif [[ -x "$ROOT_DIR/sy-opa-rules/sy-opa-rules" && "${FORCE_GO_BUILD:-}" != "1" ]]; then
    echo "sy-opa-rules binary already exists; skipping Go build (set FORCE_GO_BUILD=1 to rebuild)."
  elif ! command -v go >/dev/null 2>&1; then
    echo "Warning: go not found. Skipping sy-opa-rules build." >&2
  else
    echo "Building sy-opa-rules (Go)..."
    (cd "$ROOT_DIR/sy-opa-rules" && go build -o sy-opa-rules .)
  fi
fi

sudo install -d "$CONFIG_DIR"
sudo install -d "$STATE_DIR"
sudo install -d -m 0700 "$STATE_DIR/ssh"
sudo install -d "$STATE_DIR/state/nodes"
sudo install -d "$STATE_DIR/hives"
sudo install -d "$STATE_DIR/opa"
sudo install -d "$STATE_DIR/opa/current"
sudo install -d "$STATE_DIR/opa/staged"
sudo install -d "$STATE_DIR/opa/backup"
sudo install -d "$STATE_DIR/modules"
sudo install -d "$STATE_DIR/blob"
sudo install -d "$STATE_DIR/syncthing"
sudo install -d "$STATE_DIR/vendor"
sudo install -d "$STATE_DIR/vendor/bin"
sudo install -d "$STATE_DIR/nats"
sudo install -d "$STATE_DIR/dist"
sudo install -d "$STATE_DIR/dist/runtimes"
sudo install -d "$STATE_DIR/dist/core"
sudo install -d "$STATE_DIR/dist/core/bin"
sudo install -d "$STATE_DIR/dist/vendor"
sudo install -d "$STATE_DIR/dist/vendor/syncthing"
sudo install -d "$RUN_DIR"
sudo install -d "$RUN_DIR/routers"

MOTHERBEE_KEY="$STATE_DIR/ssh/motherbee.key"
MOTHERBEE_KEY_PUB="$STATE_DIR/ssh/motherbee.key.pub"
if [[ ! -f "$MOTHERBEE_KEY" ]]; then
  echo "Generating motherbee SSH key at $MOTHERBEE_KEY"
  sudo ssh-keygen -t ed25519 -N "" -f "$MOTHERBEE_KEY" >/dev/null
fi
sudo chmod 700 "$STATE_DIR/ssh"
sudo chmod 600 "$MOTHERBEE_KEY"
if [[ -f "$MOTHERBEE_KEY_PUB" ]]; then
  sudo chmod 644 "$MOTHERBEE_KEY_PUB"
fi

if [[ "${SKIP_BUILD:-}" == "1" ]]; then
  echo "SKIP_BUILD=1: installing only binaries from $BIN_DIR" >&2
fi

echo "Installing binaries to /usr/bin from $BIN_DIR..."

pick_bin() {
  local name="$1"
  if [[ -f "$BIN_DIR/$name" ]]; then
    echo "$BIN_DIR/$name"
    return 0
  fi
  return 1
}

missing=0
json_router_bin="$(pick_bin json-router)" || { echo "Missing binary: $BIN_DIR/json-router" >&2; missing=1; }
sy_admin_bin="$(pick_bin sy_admin)" || { echo "Missing binary: $BIN_DIR/sy_admin" >&2; missing=1; }
sy_config_bin="$(pick_bin sy_config_routes)" || { echo "Missing binary: $BIN_DIR/sy_config_routes" >&2; missing=1; }
sy_architect_bin="$(pick_bin sy_architect)" || { echo "Missing binary: $BIN_DIR/sy_architect" >&2; missing=1; }
sy_orch_bin="$(pick_bin sy_orchestrator)" || { echo "Missing binary: $BIN_DIR/sy_orchestrator" >&2; missing=1; }
sy_storage_bin="$(pick_bin sy_storage)" || { echo "Missing binary: $BIN_DIR/sy_storage" >&2; missing=1; }
sy_identity_bin="$(pick_bin sy_identity)" || { echo "Missing binary: $BIN_DIR/sy_identity" >&2; missing=1; }
sy_cognition_bin="$(pick_bin sy_cognition)" || { echo "Missing binary: $BIN_DIR/sy_cognition" >&2; missing=1; }
sy_frontdesk_gov_bin="$(pick_bin sy-frontdesk-gov)" || { echo "Missing binary: $BIN_DIR/sy-frontdesk-gov" >&2; missing=1; }
sy_opa_rules_bin=""
if [[ -f "$ROOT_DIR/sy-opa-rules/sy-opa-rules" ]]; then
  sy_opa_rules_bin="$ROOT_DIR/sy-opa-rules/sy-opa-rules"
elif sy_opa_rules_bin="$(pick_bin sy_opa_rules || true)"; then
  :
fi
if [[ -z "${sy_opa_rules_bin:-}" ]]; then
  echo "Missing binary: $ROOT_DIR/sy-opa-rules/sy-opa-rules or $BIN_DIR/sy_opa_rules" >&2
  missing=1
fi

if [[ "$missing" -eq 1 ]]; then
  echo "Build them first (e.g. cargo build --release --bins) or set BIN_DIR to where they exist." >&2
  exit 1
fi

sudo install -m 0755 "$json_router_bin" /usr/bin/rt-gateway
sudo install -m 0755 "$sy_admin_bin" /usr/bin/sy-admin
sudo install -m 0755 "$sy_config_bin" /usr/bin/sy-config-routes
sudo install -m 0755 "$sy_architect_bin" /usr/bin/sy-architect
sudo install -m 0755 "$sy_orch_bin" /usr/bin/sy-orchestrator
sudo install -m 0755 "$sy_storage_bin" /usr/bin/sy-storage
sudo install -m 0755 "$sy_identity_bin" /usr/bin/sy-identity
sudo install -m 0755 "$sy_cognition_bin" /usr/bin/sy-cognition
sudo install -m 0755 "$sy_opa_rules_bin" /usr/bin/sy-opa-rules
sudo install -m 0755 "$sy_frontdesk_gov_bin" /usr/bin/sy-frontdesk-gov

echo "Updating core source repo in $STATE_DIR/dist/core/bin..."
sudo install -m 0755 "$json_router_bin" "$STATE_DIR/dist/core/bin/rt-gateway"
sudo install -m 0755 "$sy_admin_bin" "$STATE_DIR/dist/core/bin/sy-admin"
sudo install -m 0755 "$sy_config_bin" "$STATE_DIR/dist/core/bin/sy-config-routes"
sudo install -m 0755 "$sy_architect_bin" "$STATE_DIR/dist/core/bin/sy-architect"
sudo install -m 0755 "$sy_orch_bin" "$STATE_DIR/dist/core/bin/sy-orchestrator"
sudo install -m 0755 "$sy_storage_bin" "$STATE_DIR/dist/core/bin/sy-storage"
sudo install -m 0755 "$sy_identity_bin" "$STATE_DIR/dist/core/bin/sy-identity"
sudo install -m 0755 "$sy_cognition_bin" "$STATE_DIR/dist/core/bin/sy-cognition"
sudo install -m 0755 "$sy_opa_rules_bin" "$STATE_DIR/dist/core/bin/sy-opa-rules"
sudo install -m 0755 "$sy_frontdesk_gov_bin" "$STATE_DIR/dist/core/bin/sy-frontdesk-gov"

rt_gateway_sha="$(sha256sum "$STATE_DIR/dist/core/bin/rt-gateway" | awk '{print $1}')"
rt_gateway_size="$(stat -c %s "$STATE_DIR/dist/core/bin/rt-gateway")"
sy_admin_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-admin" | awk '{print $1}')"
sy_admin_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-admin")"
sy_config_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-config-routes" | awk '{print $1}')"
sy_config_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-config-routes")"
sy_architect_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-architect" | awk '{print $1}')"
sy_architect_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-architect")"
sy_opa_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-opa-rules" | awk '{print $1}')"
sy_opa_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-opa-rules")"
sy_orch_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-orchestrator" | awk '{print $1}')"
sy_orch_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-orchestrator")"
sy_storage_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-storage" | awk '{print $1}')"
sy_storage_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-storage")"
sy_cognition_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-cognition" | awk '{print $1}')"
sy_cognition_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-cognition")"
sy_frontdesk_gov_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-frontdesk-gov" | awk '{print $1}')"
sy_frontdesk_gov_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-frontdesk-gov")"
core_version="${FLUXBEE_CORE_VERSION:-dev}"
if [[ -n "${FLUXBEE_CORE_BUILD_ID:-}" ]]; then
  core_build_id="$FLUXBEE_CORE_BUILD_ID"
elif command -v git >/dev/null 2>&1; then
  core_build_id="$(git -C "$ROOT_DIR" rev-parse --short HEAD 2>/dev/null || true)"
fi
if [[ -z "${core_build_id:-}" ]]; then
  core_build_id="$(date -u +%Y%m%d%H%M%S)"
fi
sy_identity_sha="$(sha256sum "$STATE_DIR/dist/core/bin/sy-identity" | awk '{print $1}')"
sy_identity_size="$(stat -c %s "$STATE_DIR/dist/core/bin/sy-identity")"

core_manifest_tmp="$(mktemp)"
cat >"$core_manifest_tmp" <<EOF
{
  "schema_version": 1,
  "components": {
    "rt-gateway": {"service": "rt-gateway", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$rt_gateway_sha", "size": $rt_gateway_size},
    "sy-admin": {"service": "sy-admin", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_admin_sha", "size": $sy_admin_size},
    "sy-config-routes": {"service": "sy-config-routes", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_config_sha", "size": $sy_config_size},
    "sy-architect": {"service": "sy-architect", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_architect_sha", "size": $sy_architect_size},
    "sy-opa-rules": {"service": "sy-opa-rules", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_opa_sha", "size": $sy_opa_size},
    "sy-identity": {"service": "sy-identity", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_identity_sha", "size": $sy_identity_size},
    "sy-orchestrator": {"service": "sy-orchestrator", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_orch_sha", "size": $sy_orch_size},
    "sy-storage": {"service": "sy-storage", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_storage_sha", "size": $sy_storage_size},
    "sy-cognition": {"service": "sy-cognition", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_cognition_sha", "size": $sy_cognition_size},
    "sy-frontdesk-gov": {"service": "sy-frontdesk-gov", "version": "$core_version", "build_id": "$core_build_id", "sha256": "$sy_frontdesk_gov_sha", "size": $sy_frontdesk_gov_size}
  }
}
EOF
sudo install -m 0644 "$core_manifest_tmp" "$STATE_DIR/dist/core/manifest.json"
rm -f "$core_manifest_tmp"
echo "Updated core manifest at $STATE_DIR/dist/core/manifest.json"

verify_core_component() {
  local component="$1"
  local expected_sha="$2"
  local expected_size="$3"
  local dist_path="$STATE_DIR/dist/core/bin/$component"
  local usr_path="/usr/bin/$component"

  if [[ ! -f "$dist_path" ]]; then
    echo "Error: missing core dist binary: $dist_path" >&2
    exit 1
  fi
  if [[ ! -f "$usr_path" ]]; then
    echo "Error: missing installed core binary: $usr_path" >&2
    exit 1
  fi

  local dist_sha dist_size usr_sha usr_size
  dist_sha="$(sha256sum "$dist_path" | awk '{print $1}')"
  dist_size="$(stat -c %s "$dist_path")"
  usr_sha="$(sha256sum "$usr_path" | awk '{print $1}')"
  usr_size="$(stat -c %s "$usr_path")"

  if [[ "$dist_sha" != "$expected_sha" || "$dist_size" != "$expected_size" ]]; then
    echo "Error: dist/core/bin mismatch for $component (expected sha=$expected_sha size=$expected_size, got sha=$dist_sha size=$dist_size)" >&2
    exit 1
  fi
  if [[ "$usr_sha" != "$expected_sha" || "$usr_size" != "$expected_size" ]]; then
    echo "Error: /usr/bin mismatch for $component (expected sha=$expected_sha size=$expected_size, got sha=$usr_sha size=$usr_size)" >&2
    exit 1
  fi
}

echo "Verifying installed core binaries (dist/core/bin + /usr/bin)..."
verify_core_component "rt-gateway" "$rt_gateway_sha" "$rt_gateway_size"
verify_core_component "sy-admin" "$sy_admin_sha" "$sy_admin_size"
verify_core_component "sy-config-routes" "$sy_config_sha" "$sy_config_size"
verify_core_component "sy-architect" "$sy_architect_sha" "$sy_architect_size"
verify_core_component "sy-opa-rules" "$sy_opa_sha" "$sy_opa_size"
verify_core_component "sy-identity" "$sy_identity_sha" "$sy_identity_size"
verify_core_component "sy-orchestrator" "$sy_orch_sha" "$sy_orch_size"
verify_core_component "sy-storage" "$sy_storage_sha" "$sy_storage_size"
verify_core_component "sy-cognition" "$sy_cognition_sha" "$sy_cognition_size"
verify_core_component "sy-frontdesk-gov" "$sy_frontdesk_gov_sha" "$sy_frontdesk_gov_size"
echo "Core binaries verification passed."

seeded_syncthing_vendor=0
candidate_syncthing_vendor=""

for candidate in \
  "$ROOT_DIR/vendor/syncthing/syncthing" \
  "$ROOT_DIR/vendor/syncthing/linux-amd64/syncthing" \
  "$ROOT_DIR/vendor/syncthing-linux-amd64-v2.0.14/syncthing"
do
  if [[ -x "$candidate" ]]; then
    candidate_syncthing_vendor="$candidate"
    break
  fi
done

if [[ -z "$candidate_syncthing_vendor" ]]; then
  for candidate in "$ROOT_DIR"/vendor/*/syncthing; do
    if [[ -x "$candidate" ]]; then
      candidate_syncthing_vendor="$candidate"
      break
    fi
  done
fi

if [[ -n "$candidate_syncthing_vendor" ]]; then
  sudo install -m 0755 "$candidate_syncthing_vendor" "$STATE_DIR/dist/vendor/syncthing/syncthing"
  seeded_syncthing_vendor=1
  echo "Seeded Syncthing vendor binary from $candidate_syncthing_vendor"
fi

if [[ "$seeded_syncthing_vendor" -eq 0 ]] && command -v syncthing >/dev/null 2>&1; then
  syncthing_cmd="$(command -v syncthing)"
  if [[ -x "$syncthing_cmd" ]]; then
    sudo install -m 0755 "$syncthing_cmd" "$STATE_DIR/dist/vendor/syncthing/syncthing"
    seeded_syncthing_vendor=1
    echo "Seeded Syncthing vendor binary from $syncthing_cmd"
  fi
fi
if [[ "$seeded_syncthing_vendor" -eq 0 ]]; then
  echo "Warning: Syncthing vendor binary not found in vendor/. Put it at vendor/syncthing/syncthing or vendor/<bundle>/syncthing, or install syncthing before running orchestrator blob sync." >&2
fi

if [[ "$SEED_RUNTIME_FIXTURE" == "1" ]]; then
  echo "Seeding runtime fixture in $STATE_DIR/dist/runtimes: $RUNTIME_FIXTURE_NAME@$RUNTIME_FIXTURE_VERSION"
  runtime_fixture_dir="$STATE_DIR/dist/runtimes/$RUNTIME_FIXTURE_NAME/$RUNTIME_FIXTURE_VERSION"
  runtime_fixture_start="$runtime_fixture_dir/bin/start.sh"
  sudo install -d "$runtime_fixture_dir/bin"
  cat <<EOF | sudo tee "$runtime_fixture_start" >/dev/null
#!/usr/bin/env bash
set -euo pipefail
exec /bin/sleep "$RUNTIME_FIXTURE_SLEEP_SECS"
EOF
  sudo chmod 0755 "$runtime_fixture_start"

  if command -v python3 >/dev/null 2>&1; then
    runtime_manifest_tmp="$(mktemp)"
    cat <<EOF | python3 - "$STATE_DIR/dist/runtimes/manifest.json" "$RUNTIME_FIXTURE_NAME" "$RUNTIME_FIXTURE_VERSION" >"$runtime_manifest_tmp"
import json
import pathlib
import sys
import time

manifest_path = pathlib.Path(sys.argv[1])
runtime_name = sys.argv[2]
runtime_version = sys.argv[3]

doc = {
    "schema_version": 1,
    "version": 0,
    "updated_at": None,
    "runtimes": {},
}

if manifest_path.exists():
    try:
        loaded = json.loads(manifest_path.read_text(encoding="utf-8"))
        if isinstance(loaded, dict):
            doc.update(loaded)
    except Exception:
        pass

schema_version = doc.get("schema_version")
if not isinstance(schema_version, int) or schema_version < 1:
    schema_version = 1
doc["schema_version"] = schema_version
doc["version"] = int(time.time() * 1000)
doc["updated_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

runtimes = doc.get("runtimes")
if not isinstance(runtimes, dict):
    runtimes = {}
doc["runtimes"] = runtimes

entry = runtimes.get(runtime_name)
if not isinstance(entry, dict):
    entry = {}

available = entry.get("available")
if not isinstance(available, list):
    available = []
available = [str(v) for v in available if str(v).strip()]
if runtime_version not in available:
    available.append(runtime_version)

entry["available"] = sorted(set(available))
entry["current"] = runtime_version
runtimes[runtime_name] = entry

print(json.dumps(doc, indent=2, sort_keys=True))
EOF
    sudo install -m 0644 "$runtime_manifest_tmp" "$STATE_DIR/dist/runtimes/manifest.json"
    rm -f "$runtime_manifest_tmp"
    echo "Updated runtime manifest at $STATE_DIR/dist/runtimes/manifest.json (seeded $RUNTIME_FIXTURE_NAME@$RUNTIME_FIXTURE_VERSION)"
  else
    echo "Warning: python3 not found; skipping runtime manifest seed in $STATE_DIR/dist/runtimes/manifest.json" >&2
  fi
fi

if [[ -f "$ROOT_DIR/config/hive.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/hive.yaml" "$CONFIG_DIR/hive.yaml"
fi

if [[ -f "$ROOT_DIR/config/sy-config-routes.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/sy-config-routes.yaml" "$CONFIG_DIR/sy-config-routes.yaml"
else
  if [[ ! -f "$CONFIG_DIR/sy-config-routes.yaml" ]]; then
    echo "Creating default $CONFIG_DIR/sy-config-routes.yaml"
    cat <<'EOF' | sudo tee "$CONFIG_DIR/sy-config-routes.yaml" >/dev/null
version: 1
updated_at: ""
routes: []
vpns: []
EOF
  fi
fi

install_unit() {
  local name="$1"
  local exec="$2"
  local path="/etc/systemd/system/${name}.service"
  cat <<EOF | sudo tee "$path" >/dev/null
[Unit]
Description=Fluxbee ${name}
After=network.target

[Service]
Type=simple
ExecStart=${exec}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
}

echo "Installing systemd units..."
install_unit "rt-gateway" "/usr/bin/rt-gateway"
install_unit "sy-config-routes" "/usr/bin/sy-config-routes"
install_unit "sy-opa-rules" "/usr/bin/sy-opa-rules"
install_unit "sy-admin" "/usr/bin/sy-admin"
install_unit "sy-architect" "/usr/bin/sy-architect"
install_unit "sy-orchestrator" "/usr/bin/sy-orchestrator"
install_unit "sy-storage" "/usr/bin/sy-storage"
install_unit "sy-identity" "/usr/bin/sy-identity"
install_unit "sy-cognition" "/usr/bin/sy-cognition"
install_unit "sy-frontdesk-gov" "/usr/bin/sy-frontdesk-gov"
sudo systemctl daemon-reload

if [[ "$RESTART_ORCHESTRATOR_AFTER_INSTALL" == "1" ]]; then
  if sudo systemctl list-unit-files sy-orchestrator.service >/dev/null 2>&1; then
    if sudo systemctl is-active --quiet sy-orchestrator; then
      echo "Restarting sy-orchestrator to apply new binary..."
      sudo systemctl restart sy-orchestrator
    else
      echo "sy-orchestrator is not active; skipping restart."
    fi
  fi
else
  echo "RESTART_ORCHESTRATOR_AFTER_INSTALL=0: skipping sy-orchestrator restart."
fi

if sudo systemctl list-unit-files sy-architect.service >/dev/null 2>&1; then
  if sudo systemctl is-active --quiet sy-architect; then
    echo "Restarting sy-architect to apply new binary..."
    sudo systemctl restart sy-architect
  else
    echo "sy-architect is not active; skipping restart."
  fi
fi

if sudo systemctl list-unit-files sy-frontdesk-gov.service >/dev/null 2>&1; then
  if sudo systemctl is-active --quiet sy-frontdesk-gov; then
    echo "Restarting sy-frontdesk-gov to apply new binary..."
    sudo systemctl restart sy-frontdesk-gov
  else
    echo "sy-frontdesk-gov is not active; skipping restart."
  fi
fi

if [[ "$APPLY_DEV_OWNERSHIP" == "1" ]]; then
  echo "Applying ownership for test/dev user: $INSTALL_OWNER"
  sudo chown -R "$INSTALL_OWNER":"$INSTALL_OWNER" "$CONFIG_DIR" "$STATE_DIR" "$RUN_DIR"
  sudo chown "$INSTALL_OWNER":"$INSTALL_OWNER" "$CONFIG_DIR/sy-config-routes.yaml" "$CONFIG_DIR/hive.yaml" 2>/dev/null || true
fi

echo "Installed config to $CONFIG_DIR, binaries to /usr/bin, core source repo to $STATE_DIR/dist/core/bin, systemd units, and runtime directories."
echo "Note: fluxbee-syncthing is managed dynamically by sy-orchestrator from hive.yaml (blob.sync.*)."
