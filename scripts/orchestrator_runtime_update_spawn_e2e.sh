#!/usr/bin/env bash
set -euo pipefail

# E2E for orchestrator system messages over router:
# 1) RUNTIME_UPDATE (updates in-memory runtime manifest)
# 2) SPAWN_NODE remote (worker hive)
# 3) KILL_NODE remote (cleanup)
#
# Requires:
# - motherbee with sy-orchestrator running
# - reachable worker hive already registered (TARGET_HIVE)
# - write permissions (sudo) to stage runtime script in /var/lib/fluxbee/runtimes
#
# Usage:
#   TARGET_HIVE="worker-220" bash scripts/orchestrator_runtime_update_spawn_e2e.sh
#
# Optional:
#   ORCH_RUNTIME="wf.orch.diag"
#   ORCH_VERSION="0.0.1"
#   ORCH_TIMEOUT_SECS="45"
#   ORCH_SEND_KILL="1"
#   ORCH_UNIT="fluxbee-orch-e2e-<custom>"
#   BUILD_BIN="1"

TARGET_HIVE="${TARGET_HIVE:-worker-220}"
ORCH_RUNTIME="${ORCH_RUNTIME:-wf.orch.diag}"
ORCH_VERSION="${ORCH_VERSION:-0.0.1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-45}"
ORCH_SEND_KILL="${ORCH_SEND_KILL:-1}"
BUILD_BIN="${BUILD_BIN:-1}"
ORCH_UNIT="${ORCH_UNIT:-fluxbee-orch-e2e-$(date +%s)}"

RUNTIME_DIR="/var/lib/fluxbee/runtimes/${ORCH_RUNTIME}/${ORCH_VERSION}"
START_SCRIPT="${RUNTIME_DIR}/bin/start.sh"

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd bash
require_cmd mkdir
require_cmd tee
require_cmd chmod

echo "Preparing runtime fixture: runtime=${ORCH_RUNTIME} version=${ORCH_VERSION}"
${SUDO} mkdir -p "${RUNTIME_DIR}/bin"
cat <<'EOF' | ${SUDO} tee "${START_SCRIPT}" >/dev/null
#!/usr/bin/env bash
set -euo pipefail
# Keep process alive until kill to make SPAWN/KILL observable in orchestrator flow.
exec /bin/sleep 3600
EOF
${SUDO} chmod +x "${START_SCRIPT}"

if [[ "${BUILD_BIN}" == "1" ]]; then
  echo "Building orch_system_diag..."
  cargo build --release --bin orch_system_diag
fi

echo "Running orchestrator runtime-update + spawn/kill E2E..."
TARGET_HIVE="${TARGET_HIVE}" \
ORCH_TARGET_HIVE="${TARGET_HIVE}" \
ORCH_RUNTIME="${ORCH_RUNTIME}" \
ORCH_VERSION="${ORCH_VERSION}" \
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS}" \
ORCH_SEND_KILL="${ORCH_SEND_KILL}" \
ORCH_UNIT="${ORCH_UNIT}" \
./target/release/orch_system_diag

echo "orchestrator runtime update + spawn E2E passed."
