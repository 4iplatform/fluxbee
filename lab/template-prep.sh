#!/usr/bin/env bash
# lab/template-prep.sh — prep the base Ubuntu VM to be a clean Fluxbee lab template.
#
# Runs INSIDE the base VM (as root); then power it off and convert it to a template.
# Fixes findings F-1/F-2/F-3 of docs/audits/2026-07-22-integrated-deploy-findings.md so that
# every clone boots clean: a unique machine-id, a sound dpkg, and a spoke ready for add_hive's
# SSH bootstrap (sshd + the administrator user + PasswordAuthentication).
#
#   sudo bash template-prep.sh      # inside the base VM
#   # then: poweroff; and on Proxmox: qm template <vmid>
#
# LAB NOTE: it bakes the administrator user with the password 'magicAI' (the default of the
# add_hive scripts). Change it for anything outside the lab.
#
# ⚠️ WHAT add_hive REALLY NEEDS, and does not follow from the above on its own:
# the join authenticates as the `ssh_user` YOU GIVE IT, and that user usually comes from
# cloud-init's `ciuser` (in this lab: fluxops), NOT from the `administrator` this script creates.
# That user needs ONE of the two:
#
#   (a) RECOMMENDED: its ~/.ssh/authorized_keys with the motherbee's public key
#       (/var/lib/fluxbee/ssh/motherbee.key.pub). add_hive probes the key first and needs no
#       password. That is what "the cloud image brings an authorized key" means.
#   (b) a password, which you pass as ssh_password.
#
# With neither, the join fails with SSH_AUTH_FAILED and its message says so:
# "key probe failed and neither ssh_key nor ssh_password supplied for bootstrap".
# It cost two failed joins in the 2026-08-05 rebuild, by assuming that the password documented
# here applied to the template's user.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run it as root" >&2; exit 1; }

echo "== F-2: clean the dpkg/apt state =="
dpkg --configure -a || true
apt-get -f install -y || true
apt-get clean; rm -rf /var/lib/apt/lists/*

echo "== F-3: a spoke ready for the SSH bootstrap =="
DEBIAN_FRONTEND=noninteractive apt-get install -y openssh-server >/dev/null 2>&1 || true
systemctl enable ssh
id administrator >/dev/null 2>&1 || useradd -m -s /bin/bash administrator
usermod -aG sudo administrator
echo 'administrator ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-administrator
chmod 440 /etc/sudoers.d/90-administrator
# 00- wins over 50-cloud-init.conf (sshd takes the first match)
mkdir -p /etc/ssh/sshd_config.d
printf 'PasswordAuthentication yes\n' > /etc/ssh/sshd_config.d/00-fbi-bootstrap.conf
echo 'administrator:magicAI' | chpasswd   # <-- CHANGE it outside the lab

echo "== F-1: a unique identity per clone =="
truncate -s0 /etc/machine-id
rm -f /var/lib/dbus/machine-id && ln -s /etc/machine-id /var/lib/dbus/machine-id
cloud-init clean --logs --seed 2>/dev/null || true   # instance-id + net state
rm -f /etc/ssh/ssh_host_*                             # unique host keys per clone
truncate -s0 /root/.bash_history 2>/dev/null || true

echo "== OK: template prep done. Power it off (poweroff) and convert it to a template (qm template <vmid>). =="
