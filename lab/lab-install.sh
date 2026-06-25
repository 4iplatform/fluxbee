#!/usr/bin/env bash
# Fluxbee lab — one-shot first-boot bring-up for the motherbee role.
# Runs under systemd (PID 1). Idempotent via a marker. Everything here is
# ENVIRONMENT/DEPLOYMENT provisioning — it never modifies fluxbee source or
# scripts/install.sh, which run unchanged.
set -uo pipefail

MARKER=/var/lib/fluxbee/.lab-installed
ROLE="${FLUXBEE_LAB_ROLE:-motherbee}"
ADMIN=127.0.0.1:8080
PG_URL="postgresql://fluxbee:fluxbee@127.0.0.1:5432"   # NB: no dbname — storage/identity/cognition each apply their own
ROOT_TENANT="tnt:00000000-0000-0000-0000-000000000001"

# install.sh expects a normal login env (cargo + go on PATH, HOME/GOPATH/USER
# set). systemd one-shots get a minimal env, so provide it explicitly. The Go
# module cache baked at image-build lives under /root/go.
export HOME=/root
export PATH=/root/.cargo/bin:/usr/local/go/bin:$PATH
export GOPATH=/root/go
export GOMODCACHE=/root/go/pkg/mod
export USER=root
cd /opt/fluxbee

if [[ -f "$MARKER" ]]; then
  echo "lab-install: already provisioned (role=$(cat "$MARKER" 2>/dev/null)); skipping."
  exit 0
fi

set -e

echo "lab-install: [1/6] lab deployment config (uplinks off, admin on 0.0.0.0)..."
python3 - <<'PY'
import re
p = "config/hive.yaml"
s = open(p, encoding="utf-8").read()
s = re.sub(r"(?m)^  uplinks:\n(?:    - .*\n)+", "  uplinks: []\n", s)           # don't dial the operator's LAN
s = re.sub(r'(?m)^(admin:\n  listen: ")127\.0\.0\.1(:\d+")', r"\g<1>0.0.0.0\g<2>", s)  # publishable admin port
open(p, "w", encoding="utf-8").write(s)
PY

echo "lab-install: [2/6] PostgreSQL (storage/identity/cognition backend)..."
export DEBIAN_FRONTEND=noninteractive
if ! command -v psql >/dev/null 2>&1; then
  apt-get update -qq >/dev/null && apt-get install -y -qq postgresql >/dev/null
fi
systemctl start postgresql 2>/dev/null || pg_ctlcluster "$(ls /usr/lib/postgresql/ | head -1)" main start 2>/dev/null || true
sleep 2
FLUXBEE_DB_USER=fluxbee FLUXBEE_DB_PASSWORD=fluxbee bash scripts/fluxbee_db_bootstrap.sh

echo "lab-install: [3/6] scripts/install.sh (build no-op + units, unchanged)..."
export FLUXBEE_DB_BOOTSTRAP_ON_INSTALL=0 RESTART_ORCHESTRATOR_AFTER_INSTALL=0 \
       APPLY_DEV_OWNERSHIP=0 INSTALL_OWNER=root CLEAN_RUNTIME_VOLATILE_ON_INSTALL=1
bash scripts/install.sh >/tmp/install.log 2>&1 && echo "  install.sh OK" || { echo "  install.sh FAILED:"; tail -20 /tmp/install.log; exit 1; }

echo "lab-install: [4/6] starting sy-orchestrator (boots the SY stack; crash-loops until the DB secret lands)..."
systemctl enable --now sy-orchestrator.service

echo "lab-install: [5/6] waiting for admin/vault, then vault_put the postgres pool secret..."
for _ in $(seq 1 60); do curl -s -m3 "$ADMIN/hives" >/dev/null 2>&1 && break; sleep 2; done
put_body="{\"key\":\"storage_postgres_url\",\"value\":{\"postgres_url\":\"$PG_URL\"},\"metadata\":{\"tenant_id\":\"$ROOT_TENANT\",\"resource_type\":\"postgres\"}}"
for _ in $(seq 1 20); do
  resp="$(curl -s -m10 -X POST "$ADMIN/hives/motherbee/vault/secrets" -H 'content-type: application/json' -d "$put_body" 2>/dev/null || true)"
  case "$resp" in *'"status":"ok"'*) echo "  vault_put OK: $resp"; break;; *) sleep 3;; esac
done

echo "lab-install: [6/6] waiting for hive ready..."
for _ in $(seq 1 90); do
  if curl -s -m3 "$ADMIN/hives" 2>/dev/null | grep -q '"status":"ok"'; then
    echo "  hive ready: $(curl -s -m3 "$ADMIN/hives")"
    break
  fi
  sleep 2
done

echo "$ROLE" > "$MARKER"
echo "lab-install: done (role=$ROLE)."
