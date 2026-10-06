#!/usr/bin/env python3
"""Host posture check (docs/host-posture-and-exposure-spec-v1.md §6). Read-only: it reports the
host's Syncthing peers, listen addresses and sync completion, and judges them against the stage-2
target (§3.5). Run it on every hive through ops.py, e.g.

    python3 lab/ops.py --env scratchpad/pve.env --pve-host 192.168.8.207 run 100 lab/posture-check.py

Stage 2 target
  motherbee  every remote device is a registered hive, with address `dynamic` (accept-only);
             every registry entry names its device; each device shares exactly its role's
             folders (A-50: blob/active only with workers); completion 100% for every folder
             and device
  spoke      exactly one remote device, the motherbee, at tcp://<wan uplink host>:22000; no
             folder offered by the motherbee left pending

A hive that is stopped on purpose (the throwaway worker kept on an old release for the stage-3
skip-upgrade test) is named with --offline HIVE: its addresses and folders are still judged, its
completion is printed but not required.

It never prints the Syncthing API key. Device ids are shortened to their first block.
"""
import glob
import json
import os
import re
import socket
import subprocess
import sys
import urllib.request

SYNC_DIR = "/var/lib/fluxbee/syncthing"
HIVE_YAML = "/etc/fluxbee/hive.yaml"
HIVES_ROOT = "/var/lib/fluxbee/hives"
PRIMARY = "motherbee"
# The folders the motherbee shares with each spoke role (an oracle, kept independent of the code).
ROLE_FOLDERS = {
    "worker": {"fluxbee-blob", "fluxbee-dist-core-worker", "fluxbee-dist-vendor",
               "fluxbee-dist-policy", "fluxbee-dist-runtimes"},
    "ingress": {"fluxbee-blob-public", "fluxbee-dist-core-ingress", "fluxbee-dist-vendor",
                "fluxbee-dist-policy"},
    "egress": {"fluxbee-dist-core-egress", "fluxbee-dist-vendor", "fluxbee-dist-policy"},
}

problems = []


def short(device_id):
    return (device_id or "").split("-")[0] or "-"


def read(path):
    try:
        with open(path, encoding="utf-8") as handle:
            return handle.read()
    except OSError:
        return None


def yaml_scalar(text, key):
    """A top-level `key: value` from a YAML file, without PyYAML (not on every host)."""
    match = re.search(r"(?m)^%s:\s*\"?([^\"\s#]+)\"?" % re.escape(key), text or "")
    return match.group(1) if match else None


def first_uplink_host(text):
    """Host of the first `wan.uplinks[].address`, brackets stripped from an IPv6 literal."""
    _, found, after = (text or "").partition("uplinks:")
    match = re.search(r"address:\s*\"?([^\"\s#,}]+)\"?", after) if found else None
    if not match:
        return None
    address = match.group(1)
    if address.startswith("["):
        return address[1:].split("]", 1)[0]
    return address.rsplit(":", 1)[0]


class Syncthing:
    def __init__(self):
        config = read(os.path.join(SYNC_DIR, "config.xml")) or ""
        key = re.search(r"<apikey>([^<]+)</apikey>", config)
        gui = re.search(r"(?s)<gui\b.*?<address>([^<]+)</address>", config)
        self.key = key.group(1).strip() if key else None
        self.base = "http://%s" % (gui.group(1).strip() if gui else "127.0.0.1:8384")

    def get(self, path):
        request = urllib.request.Request(self.base + path, headers={"X-API-Key": self.key or ""})
        with urllib.request.urlopen(request, timeout=10) as reply:
            return json.loads(reply.read().decode("utf-8"))


def registry():
    """hive_id -> (syncthing_device_id, role) from the motherbee's registry."""
    out = {}
    for info in sorted(glob.glob(os.path.join(HIVES_ROOT, "*", "info.yaml"))):
        hive_id = os.path.basename(os.path.dirname(info))
        text = read(info)
        out[hive_id] = (yaml_scalar(text, "syncthing_device_id"), yaml_scalar(text, "role"))
    return out


def offline_hives(argv):
    out = set()
    for i, arg in enumerate(argv):
        if arg == "--offline" and i + 1 < len(argv):
            out.add(argv[i + 1])
    return out


def main():
    offline = offline_hives(sys.argv[1:])
    hive = read(HIVE_YAML) or ""
    role = yaml_scalar(hive, "role") or "?"
    hive_id = yaml_scalar(hive, "hive_id") or "?"
    print("posture-check host=%s hive=%s role=%s" % (socket.gethostname(), hive_id, role))

    st = Syncthing()
    if not st.key:
        print("syncthing: no API key in config.xml (not configured?)")
        return 1
    try:
        status = st.get("/rest/system/status")
        devices = st.get("/rest/config/devices")
        options = st.get("/rest/config/options")
        connections = st.get("/rest/system/connections").get("connections", {})
        folders = st.get("/rest/config/folders")
    except Exception as err:  # noqa: BLE001 - report and fail
        print("syncthing: REST unavailable: %s" % err)
        return 1

    my_id = status.get("myID")
    print("syncthing uptime_s=%s my_id=%s" % (status.get("uptime"), short(my_id)))
    print("listen %s" % ",".join(options.get("listenAddresses") or []))
    print("options global_announce=%s local_announce=%s relays=%s nat=%s" % (
        options.get("globalAnnounceEnabled"), options.get("localAnnounceEnabled"),
        options.get("relaysEnabled"), options.get("natEnabled")))

    remote = [d for d in devices if d.get("deviceID") != my_id]
    for device in devices:
        conn = connections.get(device.get("deviceID"), {})
        local = " (local)" if device.get("deviceID") == my_id else ""
        print("device %-10s id=%s addresses=%s connected=%s via=%s%s" % (
            device.get("name"), short(device.get("deviceID")), ",".join(device.get("addresses") or []),
            conn.get("connected"), conn.get("address") or "-", local))

    names = {d.get("deviceID"): d.get("name") for d in devices}
    for folder in folders:
        members = [names.get(m.get("deviceID"), short(m.get("deviceID")))
                   for m in folder.get("devices") or [] if m.get("deviceID") != my_id]
        print("folder %-28s type=%-11s devices=%s" % (folder.get("id"), folder.get("type"), ",".join(members)))
    try:
        pending = st.get("/rest/cluster/pending/folders")
    except Exception as err:  # noqa: BLE001
        pending = {}
        problems.append("pending folders unavailable: %s" % err)
    for folder_id, info in sorted(pending.items()):
        offered = ",".join(names.get(d, short(d)) for d in (info.get("offeredBy") or {}))
        print("pending folder=%s offered_by=%s" % (folder_id, offered))
        if role != PRIMARY:
            problems.append("folder %s offered by %s is pending" % (folder_id, offered))

    if role == PRIMARY:
        reg = registry()
        for name, (device_id, hive_role) in reg.items():
            print("registry %-10s role=%s syncthing_device_id=%s" % (name, hive_role, short(device_id)))
            if not device_id:
                problems.append("registry entry %s has no syncthing_device_id" % name)
                continue
            shared = {f.get("id") for f in folders
                      if any(m.get("deviceID") == device_id for m in f.get("devices") or [])}
            expected = ROLE_FOLDERS.get(hive_role)
            if expected is None:
                problems.append("registry entry %s has an unknown role %s" % (name, hive_role))
            elif shared != expected:
                problems.append("%s (%s) shares %s; its role shares %s" % (
                    name, hive_role, sorted(shared), sorted(expected)))
        reg = {name: device_id for name, (device_id, _) in reg.items()}
        # As the orchestrator does: a device is a hive's by its recorded id, or by name while the
        # entry records none.
        recorded = {device_id: name for name, device_id in reg.items() if device_id}
        by_name = {}
        for device in remote:
            by_name.setdefault(device.get("name"), []).append(device)
            claimed = device.get("deviceID") in recorded or (
                device.get("name") in reg and not reg[device.get("name")])
            if not claimed:
                problems.append("device %s (%s) is claimed by no registry entry" % (device.get("name"), short(device.get("deviceID"))))
            if (device.get("addresses") or []) != ["dynamic"]:
                problems.append("device %s is not accept-only: %s" % (device.get("name"), device.get("addresses")))
        for name, found in by_name.items():
            if len(found) > 1:
                problems.append("%d devices named %s" % (len(found), name))
        for name, device_id in reg.items():
            if device_id and not any(d.get("deviceID") == device_id for d in remote):
                problems.append("registry %s device %s is not declared" % (name, short(device_id)))
        for folder in folders:
            for member in folder.get("devices") or []:
                peer = member.get("deviceID")
                if peer == my_id:
                    continue
                try:
                    done = st.get("/rest/db/completion?folder=%s&device=%s" % (folder.get("id"), peer))
                except Exception as err:  # noqa: BLE001
                    problems.append("completion %s/%s unavailable: %s" % (folder.get("id"), names.get(peer, short(peer)), err))
                    continue
                print("completion folder=%-28s device=%-10s %s%% need_items=%s" % (
                    folder.get("id"), names.get(peer, short(peer)), done.get("completion"), done.get("needItems")))
                if done.get("completion") != 100 and names.get(peer) not in offline:
                    problems.append("folder %s on %s at %s%%" % (folder.get("id"), names.get(peer, short(peer)), done.get("completion")))
    else:
        host = first_uplink_host(hive)
        expected = ["tcp://[%s]:22000" % host if host and ":" in host else "tcp://%s:22000" % host]
        if len(remote) != 1:
            problems.append("%d remote devices, expected 1 (the motherbee)" % len(remote))
        for device in remote:
            if device.get("name") != PRIMARY:
                problems.append("remote device named %s, expected %s" % (device.get("name"), PRIMARY))
            elif (device.get("addresses") or []) != expected:
                problems.append("motherbee device at %s, expected %s" % (device.get("addresses"), expected))

    listeners = subprocess.run(["ss", "-Htulnp"], capture_output=True, text=True).stdout
    for line in listeners.splitlines():
        if '"syncthing"' in line:
            fields = line.split()
            print("socket %s %s" % (fields[0], fields[4]))

    if problems:
        for problem in problems:
            print("FAIL %s" % problem)
        return 1
    print("verdict ok (stage 2 target)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
