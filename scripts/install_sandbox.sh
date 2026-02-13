#!/usr/bin/env sh
set -eu

BASE_ETC="/etc/fluxbee"
BASE_STATE="/var/lib/fluxbee/state"
BASE_RUN="/var/run/fluxbee"
HIVE_ID="sandbox"
ROUTER_NAME="RT.primary"

mkdir -p "${BASE_ETC}/routers" \
  "${BASE_STATE}" \
  "${BASE_STATE}/nodes" \
  "${BASE_RUN}/routers"

if [ ! -f "${BASE_ETC}/hive.yaml" ]; then
  cat > "${BASE_ETC}/hive.yaml" <<EOF_HIVE
hive_id: "${HIVE_ID}"
EOF_HIVE
fi

ROUTER_DIR="${BASE_ETC}/routers/${ROUTER_NAME}@${HIVE_ID}"
mkdir -p "${ROUTER_DIR}"

if [ ! -f "${ROUTER_DIR}/config.yaml" ]; then
  cat > "${ROUTER_DIR}/config.yaml" <<EOF_ROUTER
router:
  name: ${ROUTER_NAME}
  hive_id: ${HIVE_ID}
  is_gateway: false

paths:
  state_dir: ${BASE_STATE}
  node_socket_dir: ${BASE_RUN}/routers
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000
EOF_ROUTER
fi

echo "Installed sandbox config in ${BASE_ETC}"
