#!/usr/bin/env bash
# Fluxbee lab — worker container provisioner: stand up an EMPTY Linux box that
# motherbee will bootstrap via `add_hive` over SSH. It installs NO fluxbee — only
# what the SSH bootstrap needs (an admin login with a password + sudo, sshd with
# password auth) plus PostgreSQL for the worker's SY.identity/SY.cognition DBs.
# Reuses the fluxbee-lab image; bind-mounted over /usr/local/bin/lab-install.sh
# so the baked one-shot runs this instead of the motherbee bootstrap.
set -uo pipefail

MARKER=/var/lib/.lab-worker-ready
# Lab admin credentials the operator passes to add_hive (ssh_user/ssh_password).
SSH_USER=administrator
SSH_PASS=labpass

export HOME=/root USER=root DEBIAN_FRONTEND=noninteractive

if [[ -f "$MARKER" ]]; then
  echo "worker: already provisioned; skipping."
  exit 0
fi
set -e

echo "worker: [1/2] admin SSH login '$SSH_USER' (password + sudo) for add_hive bootstrap..."
id "$SSH_USER" >/dev/null 2>&1 || useradd -m -s /bin/bash "$SSH_USER"
echo "$SSH_USER:$SSH_PASS" | chpasswd
usermod -aG sudo "$SSH_USER"
mkdir -p /etc/ssh/sshd_config.d
printf 'PasswordAuthentication yes\nKbdInteractiveAuthentication yes\nPubkeyAuthentication yes\n' \
  > /etc/ssh/sshd_config.d/00-lab.conf
systemctl restart ssh

echo "worker: [2/2] PostgreSQL (worker SY.identity + SY.cognition local DBs)..."
if ! command -v psql >/dev/null 2>&1; then
  apt-get update -qq >/dev/null && apt-get install -y -qq postgresql >/dev/null
fi
systemctl start postgresql 2>/dev/null || pg_ctlcluster "$(ls /usr/lib/postgresql/ | head -1)" main start 2>/dev/null || true
sleep 2
cd /opt/fluxbee
FLUXBEE_DB_USER=fluxbee FLUXBEE_DB_PASSWORD=fluxbee bash scripts/fluxbee_db_bootstrap.sh

echo "worker: [3/3] seeding syncthing vendor for dist-sync (mirrors scripts/install.sh)..."
ST_SRC="$(ls -d /opt/fluxbee/vendor/syncthing-linux-amd64-*/ 2>/dev/null | head -1)"
install -d /var/lib/fluxbee/dist/vendor/syncthing
install -m0755 "${ST_SRC}syncthing" /var/lib/fluxbee/dist/vendor/syncthing/syncthing
[[ -f "${ST_SRC}config.xml" ]] && install -m0644 "${ST_SRC}config.xml" /var/lib/fluxbee/dist/vendor/syncthing/config.xml
python3 - <<'PY'
import json, hashlib, os, time
base = "/var/lib/fluxbee/dist/vendor"
def comp(p, path):
    b = open(p, "rb").read()
    return {"upstream_version": "repo-seeded", "hash": "sha256:" + hashlib.sha256(b).hexdigest(), "size": len(b), "path": path}
doc = {"schema_version": 1, "version": int(time.time() * 1000),
       "updated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
       "components": {"syncthing": comp(base + "/syncthing/syncthing", "syncthing/syncthing")}}
cfg = base + "/syncthing/config.xml"
if os.path.exists(cfg):
    doc["components"]["syncthing_config"] = comp(cfg, "syncthing/config.xml")
open(base + "/manifest.json", "w").write(json.dumps(doc, indent=2, sort_keys=True))
PY

touch "$MARKER"
echo "worker: EMPTY BOX READY (ssh_user=$SSH_USER) — awaiting add_hive from motherbee."
