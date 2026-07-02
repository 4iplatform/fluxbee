#!/usr/bin/env bash
# Clean-slate validation of the LinkedHelper adapter self-update loop on a
# pristine Proxmox VM, with the adapter installed as a systemd service. This is
# the environment that surfaces install/update *residue* bugs the local harness
# (test-adapter-update-loop.sh) cannot: stale state dirs, a running service
# across the re-exec, leftover binaries, permissions.
#
# It drives the VM through lab/pve.py (qemu-guest-agent), builds v1+v2 on the
# VM, snapshots a pristine baseline, then runs each scenario from that baseline
# and rolls back between runs so every scenario starts from an identical state.
#
# Prerequisites (see docs/onworking NOE/adapter_update_loop_validation_runbook.md):
#   - PVE_HOST + PVE_TOKEN exported (see the proxmox-test-env memory).
#   - VMID is a clean Ubuntu 24.04 clone in the `dev` pool with the guest agent.
#   - The VM has outbound internet (for rustup + crate deps).
#
# Usage: VMID=201 ADAPTER_CRATE=~/repos/fluxbee_cloud/adapters/linked-helper/adapter-rs \
#          bash scripts/test-adapter-update-loop-vm.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PVE="$HERE/../lab/pve.py"
ADAPTER_CRATE="${ADAPTER_CRATE:-$HERE/../nodes/io/adapters/linked-helper/adapter-rs}"
VMID="${VMID:?set VMID to a clean Ubuntu VM in the dev pool}"
PORT="${PORT:-8799}"
BASELINE="update-loop-baseline"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/lh-vm-XXXXXX")"
PASS=0; FAIL=0

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()  { printf '\033[1;32m  PASS\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad() { printf '\033[1;31m  FAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }
vm()  { python3 "$PVE" exec "$VMID" -- "$@"; }
trap 'rm -rf "$WORK"' EXIT

# --- 0. reachability ---------------------------------------------------------
log "waiting for guest agent on VM $VMID"
python3 "$PVE" wait-agent "$VMID"

# --- 1. stage crate source + mock, build v1+v2 on the VM ---------------------
log "staging crate source + mock cloud onto the VM"
tar czf "$WORK/crate.tgz" -C "$(dirname "$ADAPTER_CRATE")" \
  --exclude adapter-rs/target --exclude '.linkedhelper-adapter-state*' \
  "$(basename "$ADAPTER_CRATE")"
# pve.py agent file-write only accepts text, so ship the tarball base64-encoded
# and decode it on the VM.
base64 < "$WORK/crate.tgz" > "$WORK/crate.tgz.b64"
python3 "$PVE" push "$VMID" "$WORK/crate.tgz.b64" /tmp/lh-crate.tgz.b64
vm 'bash -lc "base64 -d /tmp/lh-crate.tgz.b64 > /tmp/lh-crate.tgz"'
# The mock script has non-ASCII chars (Proxmox file-write rejects wide chars),
# so ship it base64-encoded too.
base64 < "$HERE/adapter-update-mock-cloud.py" > "$WORK/mock.py.b64"
python3 "$PVE" push "$VMID" "$WORK/mock.py.b64" /tmp/adapter-update-mock-cloud.py.b64
vm 'bash -lc "base64 -d /tmp/adapter-update-mock-cloud.py.b64 > /tmp/adapter-update-mock-cloud.py"'

# The guest-agent exec env has no $HOME/minimal PATH, and the minimal template
# lacks a C linker (rusqlite compiles bundled sqlite). Set both explicitly.
ENVPRE='export HOME=/root PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin;'

log "ensuring build toolchain (C linker + Rust >= 1.85 for edition 2024) on the VM"
vm "bash -lc '$ENVPRE export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq build-essential curl >/dev/null'"
vm "bash -lc '$ENVPRE if ! cargo --version >/dev/null 2>&1; then curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal; fi; cargo --version'"

# Built binaries + mock live in /opt/lh-test (on disk, so they survive the
# snapshot/rollback — /tmp is tmpfs and is empty after a reboot).
log "building v1 (current), v2 (0.2.0), and v2crash (0.2.0 + test-crash-on-boot) on the VM"
vm "bash -lc '$ENVPRE set -e; mkdir -p /opt/lh-test; \
  rm -rf /tmp/b && mkdir -p /tmp/b && tar xzf /tmp/lh-crate.tgz -C /tmp/b && cd /tmp/b/adapter-rs && \
  cargo build --release --quiet && cp target/release/adapter-rs /opt/lh-test/adapter-rs.v1 && \
  sed \"s/^version = \\\"0.1.0\\\"/version = \\\"0.2.0\\\"/\" Cargo.toml > Cargo.toml.new && mv Cargo.toml.new Cargo.toml && \
  cargo build --release --quiet && cp target/release/adapter-rs /opt/lh-test/adapter-rs.v2 && \
  cargo build --release --quiet --features test-crash-on-boot && cp target/release/adapter-rs /opt/lh-test/adapter-rs.v2crash'"

# --- 2. install adapter as a systemd service + mock, then snapshot baseline --
log "installing adapter (v1) as a systemd service + mock cloud unit"
vm 'bash -lc "set -e; \
  install -d /usr/local/lib/lh-adapter /var/lib/lh-adapter /opt/lh-test; \
  cp /tmp/adapter-update-mock-cloud.py /opt/lh-test/adapter-update-mock-cloud.py; \
  cp /opt/lh-test/adapter-rs.v1 /usr/local/lib/lh-adapter/adapter-rs; \
  chmod 0755 /usr/local/lib/lh-adapter/adapter-rs"'

# mock-cloud unit (serves the v2 artifact + update directive). LH_DIRECTIVE is
# flipped per scenario by run_scenario; systemd expands it at start time.
cat > "$WORK/lh-mock-cloud.service" <<EOF
[Unit]
Description=LH adapter update mock cloud
[Service]
ExecStart=/usr/bin/python3 /opt/lh-test/adapter-update-mock-cloud.py --port $PORT --bind 127.0.0.1 --artifact /opt/lh-test/lh-serve-artifact --version 0.2.0 --directive \${LH_DIRECTIVE}
Environment=LH_DIRECTIVE=none
Restart=no
[Install]
WantedBy=multi-user.target
EOF

# adapter service unit (run loop; re-exec swaps this same process image in place)
cat > "$WORK/lh-adapter.service" <<EOF
[Unit]
Description=LinkedHelper adapter
After=lh-mock-cloud.service
# No rate limiting: the adapter's boot-gate (max 3 boots) is the crash-loop
# guard, so systemd must keep restarting long enough for it to roll back.
StartLimitIntervalSec=0
[Service]
ExecStart=/usr/local/lib/lh-adapter/adapter-rs --state-file /var/lib/lh-adapter/state.json run --interval-seconds 10
Restart=always
RestartSec=3
[Install]
WantedBy=multi-user.target
EOF

python3 "$PVE" push "$VMID" "$WORK/lh-mock-cloud.service" /etc/systemd/system/lh-mock-cloud.service
python3 "$PVE" push "$VMID" "$WORK/lh-adapter.service" /etc/systemd/system/lh-adapter.service
vm 'bash -lc "systemctl daemon-reload"'
log "snapshotting pristine baseline: $BASELINE"
python3 "$PVE" snapshot "$VMID" "$BASELINE"

# --- scenario runner ---------------------------------------------------------
# $1=directive  $2=served-artifact (v2 | v2crash)  $3=wait-seconds
run_scenario() {
  local directive="$1" artifact="$2" wait_s="${3:-25}"
  vm 'bash -lc "systemctl stop lh-adapter lh-mock-cloud 2>/dev/null; rm -f /var/lib/lh-adapter/state.json*"'
  # The mock computes the artifact sha at startup, so stage the served bytes
  # (v2 for the good/bad paths, v2crash for the crash-loop path) before start.
  vm "bash -lc 'cp /opt/lh-test/adapter-rs.$artifact /opt/lh-test/lh-serve-artifact'"
  vm "bash -lc 'sed -i \"s/^Environment=LH_DIRECTIVE=.*/Environment=LH_DIRECTIVE=$directive/\" /etc/systemd/system/lh-mock-cloud.service; systemctl daemon-reload'"
  vm 'bash -lc "systemctl start lh-mock-cloud; sleep 1; \
    /usr/local/lib/lh-adapter/adapter-rs --state-file /var/lib/lh-adapter/state.json enroll --cloud http://127.0.0.1:'"$PORT"' --token dummy >/dev/null; \
    systemctl start lh-adapter"'
  sleep "$wait_s"
}
status_get() { # $1=python-expr
  vm 'bash -lc "/usr/local/lib/lh-adapter/adapter-rs --state-file /var/lib/lh-adapter/state.json status"' \
    | python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"
}

restore_baseline() {
  log "rolling back to $BASELINE"
  python3 "$PVE" rollback "$VMID" "$BASELINE"
  # A disk-only snapshot leaves the VM stopped after rollback; start it (tolerant
  # if some Proxmox versions resume it) and wait for the agent.
  python3 "$PVE" start "$VMID" 2>/dev/null || true
  python3 "$PVE" wait-agent "$VMID"
}

# --- 3. GOOD: required + correct sha -> upgrade, service healthy -------------
log "scenario GOOD (required, correct sha)"
run_scenario required v2 25
V="$(status_get 'd["state"]["adapterVersion"]')"
R="$(status_get 'd["state"]["runtime"].get("lastUpdate",{}).get("result")')"
ACTIVE="$(vm 'bash -lc "systemctl is-active lh-adapter"' | tr -d '[:space:]')"
[ "$V" = "0.2.0" ] && ok "upgraded to $V" || bad "expected 0.2.0, got '$V'"
[ "$R" = "success" ] && ok "lastUpdate.result=success" || bad "expected success, got '$R'"
[ "$ACTIVE" = "active" ] && ok "service stayed active across restart" || bad "service not active: '$ACTIVE'"
restore_baseline

# --- 4. BAD: required + wrong sha -> rejected, stays v1 ----------------------
log "scenario BAD (required, wrong sha)"
run_scenario required-badsha v2 25
V="$(status_get 'd["state"]["adapterVersion"]')"
R="$(status_get 'd["state"]["runtime"].get("lastUpdate",{}).get("result")')"
[ "$V" = "0.1.0" ] && ok "stayed on $V" || bad "expected 0.1.0, got '$V'"
[ "$R" = "failed" ] && ok "lastUpdate.result=failed" || bad "expected failed, got '$R'"
restore_baseline

# --- 5. CRASH-LOOP: required + valid sha, but v2 crashes on boot -> rollback -
# The artifact verifies and swaps in, then crash-loops; the boot-gate must
# restore v1 after MAX boots and the service must come back healthy on v1.
log "scenario CRASH-LOOP (valid v2 that crashes on boot -> supervised rollback)"
run_scenario required v2crash 55
V="$(status_get 'd["state"]["adapterVersion"]')"
R="$(status_get 'd["state"]["runtime"].get("lastUpdate",{}).get("result")')"
ACTIVE="$(vm 'bash -lc "systemctl is-active lh-adapter"' | tr -d '[:space:]')"
[ "$V" = "0.1.0" ] && ok "rolled back to $V" || bad "expected 0.1.0, got '$V'"
[ "$R" = "rolled_back" ] && ok "lastUpdate.result=rolled_back" || bad "expected rolled_back, got '$R'"
[ "$ACTIVE" = "active" ] && ok "service healthy on the restored binary" || bad "service not active: '$ACTIVE'"

log "rolling back to $BASELINE (leaving VM at pristine baseline)"
python3 "$PVE" rollback "$VMID" "$BASELINE"

echo
log "RESULT: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
