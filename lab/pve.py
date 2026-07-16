#!/usr/bin/env python3
"""pve — Proxmox VE API CLI for driving Fluxbee test VMs.

No external deps (stdlib only), real JSON parsing, and — the big one — it drives
commands *inside* VMs through the qemu-guest-agent (`exec`/`push`/`pull`), which
runs as ROOT. That means install the .deb, run fluxbee-firstboot and verify a
hive entirely through the single Proxmox API endpoint: no SSH, no expect, no
sudo, no per-VM tunnel.

Auth / config via env:
  PVE_HOST      (req)  Proxmox host/IP, e.g. 192.168.4.165  (or 127.0.0.1 if tunneled)
  PVE_TOKEN     (req)  API token: 'user@realm!tokenid=secret'
  PVE_NODE             node name (auto-detected if a single node)
  PVE_PORT             default 8006
  PVE_TEMPLATE         clone source VMID, default 9000
  PVE_STORAGE          default local-lvm
  PVE_POOL             default dev
  PVE_BRIDGE           default vmbr0
  PVE_VERIFY_TLS       default 0 (Proxmox ships a self-signed cert)

Examples:
  pve.py nodes
  pve.py list
  pve.py create 201 fb-mb --start            # clone template, start
  pve.py wait-agent 201
  pve.py exec 201 -- 'apt-get install -y /tmp/fluxbee.deb'
  pve.py exec 201 -- 'fluxbee-firstboot'
  pve.py exec 201 -- 'curl -s localhost:8080/hives'
  pve.py push 201 ./packaging/fluxbee-firstboot /usr/local/bin/x
  pve.py snapshot 201 clean-install
  pve.py rollback 201 clean-install
  pve.py ip 201
  pve.py destroy 201
"""
import os, sys, ssl, json, time, base64, argparse
import urllib.request, urllib.parse, urllib.error

PORT     = os.environ.get("PVE_PORT", "8006")
HOST     = os.environ.get("PVE_HOST", "")
TOKEN    = os.environ.get("PVE_TOKEN", "")
NODE_ENV = os.environ.get("PVE_NODE", "")
TEMPLATE = os.environ.get("PVE_TEMPLATE", "9000")
STORAGE  = os.environ.get("PVE_STORAGE", "local-lvm")
POOL     = os.environ.get("PVE_POOL", "dev")
BRIDGE   = os.environ.get("PVE_BRIDGE", "vmbr0")
VERIFY   = os.environ.get("PVE_VERIFY_TLS", "0").lower() not in ("0", "false", "no", "")

_ctx = ssl.create_default_context()
if not VERIFY:
    _ctx.check_hostname = False
    _ctx.verify_mode = ssl.CERT_NONE


def _need_env():
    miss = [k for k, v in (("PVE_HOST", HOST), ("PVE_TOKEN", TOKEN)) if not v]
    if miss:
        sys.exit("missing env: " + ", ".join(miss) +
                 "\nexport PVE_HOST=... PVE_TOKEN='user@realm!tokenid=secret'")


def api(method, path, data=None, timeout=30, quiet_errors=False):
    """One API call. Returns parsed 'data', or raises SystemExit with the body."""
    _need_env()
    url = "https://%s:%s/api2/json%s" % (HOST, PORT, path)
    body, headers = None, {"Authorization": "PVEAPIToken=%s" % TOKEN}
    if data is not None:
        body = urllib.parse.urlencode(data, doseq=True).encode()
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, context=_ctx, timeout=timeout) as r:
            return json.loads(r.read().decode()).get("data")
    except urllib.error.HTTPError as e:
        if quiet_errors:
            raise
        sys.exit("API %s %s -> HTTP %s: %s" % (method, path, e.code, e.read().decode("utf-8", "replace")))
    except urllib.error.URLError as e:
        sys.exit("API %s %s -> connection error: %s\n"
                 "Is %s:%s reachable from here? If Proxmox is on a LAN this sandbox "
                 "cannot reach, tunnel it from your machine:\n"
                 "  ssh -N -L %s:%s:%s <jump-host>\n"
                 "then set PVE_HOST=127.0.0.1." % (method, path, e.reason, HOST, PORT, PORT, HOST, PORT))


def node():
    if NODE_ENV:
        return NODE_ENV
    nodes = api("GET", "/nodes") or []
    if len(nodes) == 1:
        return nodes[0]["node"]
    names = ", ".join(n["node"] for n in nodes)
    sys.exit("multiple nodes (%s) — set PVE_NODE" % names)


def wait_task(upid, timeout=600):
    if not upid:
        return
    n, t0 = node(), time.time()
    sys.stderr.write("  task %s\n" % upid)
    while True:
        st = api("GET", "/nodes/%s/tasks/%s/status" % (n, urllib.parse.quote(upid, safe="")))
        if st.get("status") == "stopped":
            ex = st.get("exitstatus")
            if ex == "OK":
                return
            sys.exit("  task failed: %s" % ex)
        if time.time() - t0 > timeout:
            sys.exit("  task timeout after %ss" % timeout)
        time.sleep(2)


# ---------------- commands ----------------

def cmd_nodes(a):
    for n in api("GET", "/nodes") or []:
        print("%-12s %-8s cpu=%.0f%% mem=%s/%s" % (
            n["node"], n.get("status", "?"), 100 * n.get("cpu", 0),
            n.get("mem", "?"), n.get("maxmem", "?")))


def cmd_list(a):
    n = node()
    vms = api("GET", "/nodes/%s/qemu" % n) or []
    for v in sorted(vms, key=lambda x: x["vmid"]):
        print("%-6s %-22s %-8s %s" % (
            v["vmid"], v.get("name", ""), v.get("status", "?"),
            "template" if v.get("template") else ""))


def cmd_status(a):
    n = node()
    st = api("GET", "/nodes/%s/qemu/%s/status/current" % (n, a.id))
    for k in ("name", "status", "qmpstatus", "uptime", "maxmem", "cpus", "ha"):
        if k in st:
            print("%-10s %s" % (k, st[k]))
    if (st.get("agent") in (1, "1")) or a.id:
        print("agent      %s" % st.get("agent", "?"))


def cmd_create(a):
    n = node()
    clone = {"newid": a.id, "name": a.name, "full": 1}
    if POOL:
        clone["pool"] = POOL
    if STORAGE:
        clone["storage"] = STORAGE
    print("clone template %s -> VM %s (%s)" % (TEMPLATE, a.id, a.name))
    upid = api("POST", "/nodes/%s/qemu/%s/clone" % (n, TEMPLATE), clone, timeout=60)
    wait_task(upid)
    cfg = {"agent": "enabled=1", "memory": a.mem, "cores": a.cores}
    if a.ip == "dhcp":
        cfg["ipconfig0"] = "ip=dhcp"
    else:
        gw = a.ip.split("/")[0].rsplit(".", 1)[0] + ".1"
        cfg["ipconfig0"] = "ip=%s,gw=%s" % (a.ip, gw)
    if a.sshkey and os.path.exists(os.path.expanduser(a.sshkey)):
        cfg["sshkeys"] = urllib.parse.quote(open(os.path.expanduser(a.sshkey)).read(), safe="")
    if a.ciuser:
        cfg["ciuser"] = a.ciuser
    api("POST", "/nodes/%s/qemu/%s/config" % (n, a.id), cfg)
    if a.disk:
        try:
            api("PUT", "/nodes/%s/qemu/%s/resize" % (n, a.id),
                {"disk": "scsi0", "size": a.disk}, quiet_errors=True)
        except urllib.error.HTTPError:
            sys.stderr.write("  (resize scsi0 skipped — check disk bus)\n")
    print("  VM %s ready (stopped)" % a.id)
    if a.start:
        cmd_start(a)


def cmd_start(a):
    n = node()
    wait_task(api("POST", "/nodes/%s/qemu/%s/status/start" % (n, a.id), {}))
    print("  started %s" % a.id)


def cmd_stop(a):
    n = node()
    act = "stop" if getattr(a, "force", False) else "shutdown"
    wait_task(api("POST", "/nodes/%s/qemu/%s/status/%s" % (n, a.id, act), {}))
    print("  %s %s" % (act, a.id))


def cmd_reboot(a):
    n = node()
    wait_task(api("POST", "/nodes/%s/qemu/%s/status/reboot" % (n, a.id), {}))
    print("  rebooted %s" % a.id)


def cmd_destroy(a):
    n = node()
    try:
        api("POST", "/nodes/%s/qemu/%s/status/stop" % (n, a.id), {}, quiet_errors=True)
    except urllib.error.HTTPError:
        pass
    time.sleep(2)
    upid = api("DELETE", "/nodes/%s/qemu/%s?purge=1&destroy-unreferenced-disks=1" % (n, a.id))
    wait_task(upid)
    print("  destroyed %s" % a.id)


def cmd_snapshot(a):
    n = node()
    name = a.name or ("snap-%d" % int(time.time()))
    upid = api("POST", "/nodes/%s/qemu/%s/snapshot" % (n, a.id),
               {"snapname": name, "vmstate": 1 if a.vmstate else 0})
    wait_task(upid)
    print("  snapshot %s of %s" % (name, a.id))


def cmd_rollback(a):
    n = node()
    upid = api("POST", "/nodes/%s/qemu/%s/snapshot/%s/rollback" % (n, a.id, a.name), {})
    wait_task(upid)
    print("  rolled back %s -> %s" % (a.id, a.name))


def cmd_snapshots(a):
    n = node()
    for s in api("GET", "/nodes/%s/qemu/%s/snapshot" % (n, a.id)) or []:
        print("%-24s %s" % (s.get("name", ""), s.get("description", "")))


def cmd_ip(a):
    n = node()
    try:
        r = api("GET", "/nodes/%s/qemu/%s/agent/network-get-interfaces" % (n, a.id), quiet_errors=True)
    except urllib.error.HTTPError:
        sys.exit("  agent not responding yet (boot + qemu-guest-agent needed) — try wait-agent")
    for iface in (r or {}).get("result", []):
        for ad in iface.get("ip-addresses", []) or []:
            ip = ad.get("ip-address", "")
            if ad.get("ip-address-type") == "ipv4" and not ip.startswith("127."):
                print(ip)


def cmd_wait_agent(a):
    n, t0 = node(), time.time()
    while True:
        try:
            api("POST", "/nodes/%s/qemu/%s/agent/ping" % (n, a.id), {}, timeout=8, quiet_errors=True)
            print("  agent up on %s" % a.id)
            return
        except (urllib.error.HTTPError, urllib.error.URLError):
            pass
        if time.time() - t0 > a.timeout:
            sys.exit("  agent did not come up within %ss" % a.timeout)
        time.sleep(3)


def _agent_exec(n, vmid, argv, input_data=None):
    """Run argv (list) in the VM via guest agent (as root). Returns (exitcode, out, err)."""
    data = {"command": argv}
    if input_data is not None:
        data["input-data"] = input_data
    started = api("POST", "/nodes/%s/qemu/%s/agent/exec" % (n, vmid), data)
    pid = started.get("pid")
    while True:
        st = api("GET", "/nodes/%s/qemu/%s/agent/exec-status?pid=%s" % (n, vmid, pid))
        if st.get("exited"):
            return st.get("exitcode", 0), st.get("out-data", ""), st.get("err-data", "")
        time.sleep(1)


def cmd_exec(a):
    n = node()
    # everything after `--` is the command; wrap in bash -lc unless --raw
    cmdline = " ".join(a.cmd)
    argv = a.cmd if a.raw else ["/bin/bash", "-lc", cmdline]
    code, out, err = _agent_exec(n, a.id, argv, a.input)
    if out:
        sys.stdout.write(out if out.endswith("\n") else out + "\n")
    if err:
        sys.stderr.write(err if err.endswith("\n") else err + "\n")
    sys.exit(code or 0)


def cmd_push(a):
    """Write a local file into the VM via agent file-write (small files only).

    Proxmox's `encode=1` means *Proxmox* base64-encodes the content we send
    before handing it to the QEMU guest agent (which decodes it). So we send the
    RAW text content, not pre-encoded — pre-encoding would double-encode and the
    file would land as a literal base64 blob.
    """
    n = node()
    raw = open(a.local, "rb").read()
    if len(raw) > 900_000:
        sys.exit("  file too big for agent file-write (%d bytes). Serve it over HTTP "
                 "and `exec <id> -- curl -o %s <url>` instead." % (len(raw), a.remote))
    try:
        content = raw.decode("utf-8")
    except UnicodeDecodeError:
        sys.exit("  binary file — agent file-write here only supports text. Use an HTTP fetch.")
    api("POST", "/nodes/%s/qemu/%s/agent/file-write" % (n, a.id),
        {"file": a.remote, "content": content, "encode": 1})
    print("  wrote %s (%d bytes) -> %s:%s" % (a.local, len(raw), a.id, a.remote))


def cmd_pull(a):
    n = node()
    r = api("GET", "/nodes/%s/qemu/%s/agent/file-read?file=%s" % (n, a.id, urllib.parse.quote(a.remote)))
    content = r.get("content", "")
    if r.get("truncated"):
        sys.stderr.write("  (truncated by agent)\n")
    if a.local:
        open(a.local, "w").write(content)
        print("  read %s:%s -> %s" % (a.id, a.remote, a.local))
    else:
        sys.stdout.write(content)


def main():
    p = argparse.ArgumentParser(prog="pve", description="Proxmox VE API CLI for Fluxbee test VMs")
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("nodes").set_defaults(fn=cmd_nodes)
    sub.add_parser("list").set_defaults(fn=cmd_list)

    s = sub.add_parser("create"); s.add_argument("id"); s.add_argument("name")
    s.add_argument("--ip", default="dhcp"); s.add_argument("--mem", default="4096")
    s.add_argument("--cores", default="2"); s.add_argument("--disk", default="20G")
    s.add_argument("--sshkey", default="~/.ssh/id_rsa.pub"); s.add_argument("--ciuser", default="fluxbee")
    s.add_argument("--start", action="store_true"); s.set_defaults(fn=cmd_create)

    for name, fn in (("start", cmd_start), ("reboot", cmd_reboot), ("status", cmd_status)):
        s = sub.add_parser(name); s.add_argument("id"); s.set_defaults(fn=fn)

    s = sub.add_parser("stop"); s.add_argument("id"); s.add_argument("--force", action="store_true"); s.set_defaults(fn=cmd_stop)
    s = sub.add_parser("destroy"); s.add_argument("id"); s.set_defaults(fn=cmd_destroy)

    s = sub.add_parser("snapshot"); s.add_argument("id"); s.add_argument("name", nargs="?")
    s.add_argument("--vmstate", action="store_true"); s.set_defaults(fn=cmd_snapshot)
    s = sub.add_parser("rollback"); s.add_argument("id"); s.add_argument("name"); s.set_defaults(fn=cmd_rollback)
    s = sub.add_parser("snapshots"); s.add_argument("id"); s.set_defaults(fn=cmd_snapshots)

    s = sub.add_parser("ip"); s.add_argument("id"); s.set_defaults(fn=cmd_ip)
    s = sub.add_parser("wait-agent"); s.add_argument("id"); s.add_argument("--timeout", type=int, default=180); s.set_defaults(fn=cmd_wait_agent)

    s = sub.add_parser("exec"); s.add_argument("id")
    s.add_argument("--raw", action="store_true", help="don't wrap in bash -lc")
    s.add_argument("--input", default=None, help="stdin for the command")
    s.add_argument("cmd", nargs=argparse.REMAINDER); s.set_defaults(fn=cmd_exec)

    s = sub.add_parser("push"); s.add_argument("id"); s.add_argument("local"); s.add_argument("remote"); s.set_defaults(fn=cmd_push)
    s = sub.add_parser("pull"); s.add_argument("id"); s.add_argument("remote"); s.add_argument("local", nargs="?"); s.set_defaults(fn=cmd_pull)

    a = p.parse_args()
    if getattr(a, "cmd", None) == "exec" and a.cmd and a.cmd[0] == "--":
        a.cmd = a.cmd[1:]
    a.fn(a)


if __name__ == "__main__":
    main()
