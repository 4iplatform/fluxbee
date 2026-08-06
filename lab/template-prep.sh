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
#
# ⚠️ LO QUE add_hive NECESITA DE VERDAD, y no coincide automaticamente con lo de arriba:
# el join autentica contra el `ssh_user` QUE VOS LE PASES, y ese usuario suele venir del
# `ciuser` de cloud-init (en este lab: fluxops), NO del `administrator` que crea este script.
# Para ese usuario hace falta UNA de las dos:
#
#   (a) RECOMENDADO — su ~/.ssh/authorized_keys con la clave publica de motherbee
#       (/var/lib/fluxbee/ssh/motherbee.key.pub). add_hive sondea key-first y no necesita
#       password. Es lo que significa "la imagen cloud trae una authorized key".
#   (b) un password, que le pasas como ssh_password.
#
# Si no hay ninguna de las dos, el join falla con SSH_AUTH_FAILED y el mensaje lo dice:
# "key probe failed and neither ssh_key nor ssh_password supplied for bootstrap".
# Costo dos joins fallidos en la regeneracion del 2026-08-05 por asumir que el password
# documentado aca aplicaba al usuario del template.
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
