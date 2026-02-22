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
#   ORCH_SEND_RUNTIME_UPDATE="1"
#   ORCH_UNIT="fluxbee-orch-e2e-<custom>"
#   ORCH_ROUTE_MODE="unicast|resolve"
#   ORCH_EXPECT_SPAWN_UNREACHABLE_REASON="OPA_ERROR"
#   ORCH_REQUIRE_OPA_SHM_HEALTH="0|1"   # default: 1 for resolve, 0 for unicast
#   OPA_MAX_HEARTBEAT_AGE_MS="30000"
#   BUILD_BIN="1"

TARGET_HIVE="${TARGET_HIVE:-worker-220}"
ORCH_RUNTIME="${ORCH_RUNTIME:-wf.orch.diag}"
ORCH_VERSION="${ORCH_VERSION:-0.0.1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-45}"
ORCH_SEND_KILL="${ORCH_SEND_KILL:-1}"
ORCH_SEND_RUNTIME_UPDATE="${ORCH_SEND_RUNTIME_UPDATE:-1}"
BUILD_BIN="${BUILD_BIN:-1}"
ORCH_UNIT="${ORCH_UNIT:-fluxbee-orch-e2e-$(date +%s)}"
ORCH_ROUTE_MODE="${ORCH_ROUTE_MODE:-unicast}"
ORCH_EXPECT_SPAWN_UNREACHABLE_REASON="${ORCH_EXPECT_SPAWN_UNREACHABLE_REASON:-}"
OPA_MAX_HEARTBEAT_AGE_MS="${OPA_MAX_HEARTBEAT_AGE_MS:-30000}"
ORCH_REQUIRE_OPA_SHM_HEALTH="${ORCH_REQUIRE_OPA_SHM_HEALTH:-}"

if [[ -z "${ORCH_REQUIRE_OPA_SHM_HEALTH}" ]]; then
  if [[ "${ORCH_ROUTE_MODE}" == "resolve" ]]; then
    ORCH_REQUIRE_OPA_SHM_HEALTH="1"
  else
    ORCH_REQUIRE_OPA_SHM_HEALTH="0"
  fi
fi

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
  echo "Building orch diagnostics binaries..."
  cargo build --release --bin orch_system_diag --bin opa_shm_diag
fi

if [[ "${ORCH_REQUIRE_OPA_SHM_HEALTH}" == "1" ]]; then
  echo "Pre-check: OPA SHM health (required for resolve mode)"
  OPA_EXPECT_STATUS="${OPA_EXPECT_STATUS:-ok}" \
  OPA_MIN_VERSION="${OPA_MIN_VERSION:-1}" \
  OPA_MAX_HEARTBEAT_AGE_MS="${OPA_MAX_HEARTBEAT_AGE_MS}" \
  ./target/release/opa_shm_diag
fi

echo "Running orchestrator runtime-update + spawn/kill E2E..."
TARGET_HIVE="${TARGET_HIVE}" \
ORCH_TARGET_HIVE="${TARGET_HIVE}" \
ORCH_RUNTIME="${ORCH_RUNTIME}" \
ORCH_VERSION="${ORCH_VERSION}" \
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS}" \
ORCH_SEND_KILL="${ORCH_SEND_KILL}" \
ORCH_SEND_RUNTIME_UPDATE="${ORCH_SEND_RUNTIME_UPDATE}" \
ORCH_UNIT="${ORCH_UNIT}" \
ORCH_ROUTE_MODE="${ORCH_ROUTE_MODE}" \
ORCH_EXPECT_SPAWN_UNREACHABLE_REASON="${ORCH_EXPECT_SPAWN_UNREACHABLE_REASON}" \
./target/release/orch_system_diag

echo "orchestrator runtime update + spawn E2E passed."
