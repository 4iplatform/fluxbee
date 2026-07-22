#!/usr/bin/env bash
# lab/template-prep.sh — prep the base Ubuntu VM to be a clean Fluxbee lab template.
#
# Corre DENTRO de la VM base (como root), después apagá y convertí a template.
# Arregla los hallazgos F-1/F-2/F-3 de docs/audits/2026-07-22-integrated-deploy-findings.md
# para que cada clone bootee limpio: machine-id único, dpkg sano, y spoke listo para
# el bootstrap SSH de add_hive (sshd + usuario administrator + PasswordAuthentication).
#
#   sudo bash template-prep.sh      # dentro de la VM base
#   # luego: poweroff; y en Proxmox: qm template <vmid>
#
# NOTA LAB: hornea el usuario administrator con password 'magicAI' (default de los
# scripts de add_hive). Cambialo para cualquier cosa fuera del lab.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "corré como root" >&2; exit 1; }

echo "== F-2: limpiar estado dpkg/apt =="
dpkg --configure -a || true
apt-get -f install -y || true
apt-get clean; rm -rf /var/lib/apt/lists/*

echo "== F-3: spoke listo para bootstrap SSH =="
DEBIAN_FRONTEND=noninteractive apt-get install -y openssh-server >/dev/null 2>&1 || true
systemctl enable ssh
id administrator >/dev/null 2>&1 || useradd -m -s /bin/bash administrator
usermod -aG sudo administrator
echo 'administrator ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-administrator
chmod 440 /etc/sudoers.d/90-administrator
# 00- gana sobre el 50-cloud-init.conf (sshd es first-match)
mkdir -p /etc/ssh/sshd_config.d
printf 'PasswordAuthentication yes\n' > /etc/ssh/sshd_config.d/00-fbi-bootstrap.conf
echo 'administrator:magicAI' | chpasswd   # <-- CAMBIAR fuera del lab

echo "== F-1: identidad única por clone =="
truncate -s0 /etc/machine-id
rm -f /var/lib/dbus/machine-id && ln -s /etc/machine-id /var/lib/dbus/machine-id
cloud-init clean --logs --seed 2>/dev/null || true   # instance-id + net state
rm -f /etc/ssh/ssh_host_*                             # host keys únicas por clone
truncate -s0 /root/.bash_history 2>/dev/null || true

echo "== OK: template prep listo. Apagá (poweroff) y convertí a template (qm template <vmid>). =="
