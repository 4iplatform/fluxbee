#!/usr/bin/env bash
set -euo pipefail

if ! command -v systemctl >/dev/null 2>&1; then
  echo "Error: systemctl not found." >&2
  exit 1
fi

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
  $SUDO -v
fi

services=(
  "sy-frontdesk-gov"
  "sy-identity"
  "sy-cognition"
  "sy-timer"
  "sy-wf-rules"
  "sy-admin"
  "sy-architect"
  "sy-vault"
  "sy-storage"
  "sy-config-routes"
  "sy-opa-rules"
  "sy-edge"
  "fluxbee-syncthing"
  "rt-gateway"
)

service_exists() {
  local name="$1"
  local state
  state="$($SUDO systemctl show "${name}.service" --property=LoadState --value 2>/dev/null || true)"
  [[ -n "$state" && "$state" != "not-found" ]]
}

stop_unit() {
  local svc="$1"
  if ! service_exists "$svc"; then
    echo " - ${svc}.service not installed"
    return 0
  fi

  echo " - stopping ${svc}.service"
  $SUDO systemctl stop "${svc}.service" || true

  if $SUDO systemctl is-active --quiet "${svc}.service"; then
    echo "   force-killing ${svc}.service"
    $SUDO systemctl kill "${svc}.service" || true
    sleep 0.5
  fi
}

stop_matching_units() {
  local pattern="$1"
  local units=()
  mapfile -t units < <($SUDO systemctl list-units --all --type=service "$pattern" --no-legend --no-pager 2>/dev/null | awk '{print $1}' || true)
  if [[ "${#units[@]}" -eq 0 ]]; then
    echo " - no units matching ${pattern}"
    return 0
  fi

  for unit in "${units[@]}"; do
    [[ -n "$unit" ]] || continue
    echo " - stopping ${unit}"
    $SUDO systemctl stop "$unit" || true
    if $SUDO systemctl is-active --quiet "$unit"; then
      echo "   force-killing ${unit}"
      $SUDO systemctl kill "$unit" || true
      sleep 0.5
    fi
  done
}

kill_process_name() {
  local proc="$1"
  if ! command -v pgrep >/dev/null 2>&1; then
    return 0
  fi
  if $SUDO pgrep -x "$proc" >/dev/null 2>&1; then
    echo " - terminating process $proc"
    $SUDO pkill -TERM -x "$proc" || true
    sleep 1
    if $SUDO pgrep -x "$proc" >/dev/null 2>&1; then
      echo "   killing process $proc"
      $SUDO pkill -KILL -x "$proc" || true
    fi
  fi
}

echo "Stopping Fluxbee services..."
echo " - stopping sy-orchestrator.service first (watchdog)"
stop_unit "sy-orchestrator"

echo "Stopping managed runtime node units..."
stop_matching_units "fluxbee-node-*.service"

for svc in "${services[@]}"; do
  stop_unit "$svc"
done

echo "Cleaning residual processes..."
residual_procs=(
  "sy-frontdesk-gov"
  "sy-orchestrator"
  "sy-admin"
  "sy-architect"
  "sy-storage"
  "sy-cognition"
  "sy-timer"
  "sy-wf-rules"
  "sy-config-routes"
  "sy-opa-rules"
  "sy-identity"
  "sy-vault"
  "sy-edge"
  "rt-gateway"
  "ai_node_runner"
  "wf-generic"
  "io-api"
  "io-blob"
  "io-cloud"
  "io-slack"
  "io-wapp"
  "io-linkedhelper"
  "syncthing"
)
for proc in "${residual_procs[@]}"; do
  kill_process_name "$proc"
done

echo "Current service states:"
for svc in "sy-orchestrator" "${services[@]}"; do
  if service_exists "$svc"; then
    state="$($SUDO systemctl is-active "${svc}.service" 2>/dev/null || true)"
    echo " - ${svc}: ${state:-unknown}"
  else
    echo " - ${svc}: not-installed"
  fi
done
