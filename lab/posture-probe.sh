#!/usr/bin/env bash
# Host posture probe (docs/host-posture-and-exposure-spec-v1.md §6). From the VM it runs on, it
# opens a TCP connection to each TARGET:PORT and prints what happened, one line per port:
#   open     the connection was accepted
#   refused  RST: nothing listens there (or a reject rule)
#   timeout  no answer within 3 s: a drop rule, or no route
# Run it through ops.py, e.g. from worker1 against the motherbee:
#   python3 lab/ops.py --env scratchpad/pve.env --pve-host 192.168.8.207 \
#       run 101 lab/posture-probe.sh 10.10.10.10 3000 8080 9000
# Stage 1 expects 3000 and 8080 "refused" on the motherbee's LAN address (they listen on loopback
# only and no firewall drops yet) and 9000 "open". Stages 4 and 5 extend the expectations.
set -u
target="${1:?usage: posture-probe.sh TARGET PORT...}"
shift
[ "$#" -gt 0 ] || { echo "usage: posture-probe.sh TARGET PORT..." >&2; exit 2; }
from="$(hostname)"
for port in "$@"; do
  if out="$(timeout 3 bash -c "exec 3<>/dev/tcp/$target/$port" 2>&1)"; then
    result=open
  elif [ "$?" -eq 124 ]; then
    result=timeout
  elif printf '%s' "$out" | grep -qi "refused"; then
    result=refused
  else
    result="error: $(printf '%s' "$out" | tail -1)"
  fi
  echo "posture-probe from=$from to=$target:$port $result"
done
