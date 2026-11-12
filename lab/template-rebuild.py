#!/usr/bin/env python3
"""template-rebuild.py — rebuild the lab's Proxmox template (FINDINGS B-18), on top of lab/pve.py.

A template whose packages are old makes every clone spend its first boot in cloud-init's package
upgrade (about 11 minutes on 2026-11-12, against about 1 minute on a current template). Rebuilding
it runs that upgrade once. A valid template exists at every moment: the new one is built in a VM
BUILD and made a template, then cloned onto TEMPLATE once the old one is gone; BUILD goes last.

    python3 lab/template-rebuild.py --env scratchpad/pve.env --pve-host 192.168.8.207 all

Steps (each can run alone, in this order):
  build     clone TEMPLATE -> BUILD, a static address, start; wait for cloud-init (the upgrade)
  upgrade   apt dist-upgrade + autoremove; a reboot when the kernel changed, then autoremove again
  prep      push and run lab/template-prep.sh (machine-id, cloud-init state, SSH host keys), shut down
  templ     BUILD loses its address and becomes a template
  swap      the old TEMPLATE destroyed, BUILD cloned onto it (full) and made a template, BUILD destroyed
  verify    a clone (CHECK) boots with Proxmox's default first-boot upgrade: its cloud-init time and
            what is left to upgrade; then destroyed
  all       every step in order, stopping at the first that fails

It refuses to start when BUILD or CHECK exists, and swap refuses unless BUILD is a template. It prints
ids, states, counts and times, never a password or a token. The address must be free on the bridge.
"""
import argparse
import base64
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
pve = None  # lab/pve.py, imported after the env is loaded (it reads PVE_* at import)
A = None


def load_env(path, host):
    if path:
        for line in open(path):
            line = line.strip()
            if line.startswith("export "):
                line = line[len("export "):]
            if "=" in line and not line.startswith("#"):
                key, value = line.split("=", 1)
                os.environ[key.strip()] = value.strip().strip('"').strip("'")
    if host:
        os.environ["PVE_HOST"] = host
    global pve
    sys.path.insert(0, HERE)
    import pve as module
    pve = module


def stamp():
    return time.strftime("%H:%M:%SZ", time.gmtime())


def node():
    return pve.node()


def cfg(vmid):
    return pve.api("GET", "/nodes/%s/qemu/%s/config" % (node(), vmid))


def status(vmid):
    return pve.api("GET", "/nodes/%s/qemu/%s/status/current" % (node(), vmid)).get("status")


def exists(vmid):
    return any(str(v["vmid"]) == str(vmid) for v in pve.api("GET", "/nodes/%s/qemu" % node()))


def sh(vmid, cmd):
    return pve._agent_exec(node(), vmid, ["/usr/bin/env", "HOME=/root", "/bin/bash", "-lc", cmd])


def wait_agent(vmid, cap=600):
    t0 = time.time()
    while time.time() - t0 < cap:
        try:
            pve.api("POST", "/nodes/%s/qemu/%s/agent/ping" % (node(), vmid), {}, timeout=8, quiet_errors=True)
            return time.time() - t0
        except Exception:
            time.sleep(3)
    sys.exit("agent on %s did not come up within %s s" % (vmid, cap))


def wait_cloud_init(vmid, cap=1500):
    t0, last = time.time(), None
    while time.time() - t0 < cap:
        try:
            _code, out, _err = sh(vmid, "cloud-init status 2>/dev/null | head -1")
            st = (out or "").strip()
        except Exception:
            st = "agent-busy"
        if st != last:
            print("  [%s] cloud-init: %s (+%.0f s)" % (stamp(), st, time.time() - t0))
            last = st
        if st.startswith(("status: done", "status: error", "status: disabled")):
            return st, time.time() - t0
        time.sleep(15)
    return "timeout", time.time() - t0


def wait_stopped(vmid, cap=300):
    t0 = time.time()
    while time.time() - t0 < cap:
        if status(vmid) == "stopped":
            return time.time() - t0
        time.sleep(3)
    sys.exit("%s did not stop within %s s" % (vmid, cap))


def clone(src, dst, name):
    upid = pve.api("POST", "/nodes/%s/qemu/%s/clone" % (node(), src), {"newid": dst, "name": name, "full": 1},
                   timeout=120)
    pve.wait_task(upid)


def address():
    return {"ipconfig0": "ip=%s,gw=%s" % (A.addr, A.gw), "ciupgrade": 1}


def step_build():
    if exists(A.build):
        sys.exit("VM %s exists already: refused (look at it first)" % A.build)
    if str(cfg(A.template).get("template")) != "1":
        sys.exit("%s is not a template: refused" % A.template)
    t0 = time.time()
    clone(A.template, A.build, "fb-template-build")
    print("[%s] cloned %s -> %s in %.0f s" % (stamp(), A.template, A.build, time.time() - t0))
    pve.api("POST", "/nodes/%s/qemu/%s/config" % (node(), A.build), address())
    pve.wait_task(pve.api("POST", "/nodes/%s/qemu/%s/status/start" % (node(), A.build), {}))
    print("[%s] started %s; agent up after %.0f s" % (stamp(), A.build, wait_agent(A.build)))
    st, secs = wait_cloud_init(A.build)
    print("[%s] cloud-init %s after %.0f s (the first-boot upgrade is in it)" % (stamp(), st, secs))
    if not st.startswith("status: done"):
        sys.exit("cloud-init did not finish: BUILD left as it is")


def step_upgrade():
    t0 = time.time()
    code, out, _err = sh(A.build, "export DEBIAN_FRONTEND=noninteractive; apt-get update -q >/dev/null 2>&1; "
                                  "apt-get -y -q -o Dpkg::Options::=--force-confold dist-upgrade 2>&1 | tail -3; "
                                  "apt-get -y -q autoremove --purge 2>&1 | tail -1; "
                                  "echo upgradable=$(apt list --upgradable 2>/dev/null | grep -c upgradable); "
                                  "test -f /var/run/reboot-required && echo REBOOT-REQUIRED || echo no-reboot")
    print("[%s] dist-upgrade rc=%s in %.0f s:\n%s" % (stamp(), code, time.time() - t0, (out or "").strip()[-600:]))
    if code:
        sys.exit("dist-upgrade failed: BUILD left as it is")
    if "REBOOT-REQUIRED" in (out or ""):
        pve.wait_task(pve.api("POST", "/nodes/%s/qemu/%s/status/reboot" % (node(), A.build), {}))
        time.sleep(10)
        print("[%s] rebooted; agent up after %.0f s" % (stamp(), wait_agent(A.build)))
        _code, out, _err = sh(A.build, "export DEBIAN_FRONTEND=noninteractive; "
                                       "apt-get -y -q autoremove --purge 2>&1 | tail -1; uname -r; "
                                       "echo upgradable=$(apt list --upgradable 2>/dev/null | grep -c upgradable)")
        print("[%s] after the reboot: %s" % (stamp(), (out or "").strip()[-400:]))


def step_prep():
    raw = open(os.path.join(HERE, "template-prep.sh"), "rb").read()
    pve.api("POST", "/nodes/%s/qemu/%s/agent/file-write" % (node(), A.build),
            {"file": "/root/template-prep.sh", "content": base64.b64encode(raw).decode("ascii"), "encode": 0})
    code, out, _err = sh(A.build, "bash /root/template-prep.sh 2>&1 | tail -6; rm -f /root/template-prep.sh; sync")
    print("[%s] template-prep rc=%s:\n%s" % (stamp(), code, (out or "").strip()[-700:]))
    if code:
        sys.exit("template-prep failed: BUILD left as it is")
    pve.wait_task(pve.api("POST", "/nodes/%s/qemu/%s/status/shutdown" % (node(), A.build), {"timeout": 180}))
    print("[%s] %s stopped after %.0f s" % (stamp(), A.build, wait_stopped(A.build)))


def step_templ():
    if status(A.build) != "stopped":
        sys.exit("%s is not stopped" % A.build)
    pve.api("POST", "/nodes/%s/qemu/%s/config" % (node(), A.build), {"delete": "ipconfig0,ciupgrade"})
    pve.api("POST", "/nodes/%s/qemu/%s/template" % (node(), A.build), {})
    time.sleep(5)
    c = cfg(A.build)
    print("[%s] %s template=%s ipconfig0=%s" % (stamp(), A.build, c.get("template"), c.get("ipconfig0")))
    if str(c.get("template")) != "1":
        sys.exit("%s did not become a template" % A.build)


def step_swap():
    if str(cfg(A.build).get("template")) != "1":
        sys.exit("%s is not a template: the old %s stays" % (A.build, A.template))
    old = cfg(A.template)
    name, desc = old.get("name") or "fb-template", old.get("description") or ""
    t0 = time.time()
    pve.wait_task(pve.api("DELETE", "/nodes/%s/qemu/%s?purge=1" % (node(), A.template)))
    print("[%s] the old %s destroyed in %.0f s" % (stamp(), A.template, time.time() - t0))
    clone(A.build, A.template, name)
    note = "Rebuilt %s by lab/template-rebuild.py (FINDINGS B-18)." % time.strftime("%Y-%m-%d", time.gmtime())
    pve.api("POST", "/nodes/%s/qemu/%s/config" % (node(), A.template), {"description": (desc + "\n" + note).strip()})
    pve.api("POST", "/nodes/%s/qemu/%s/template" % (node(), A.template), {})
    time.sleep(5)
    print("[%s] %s cloned from %s and made a template: template=%s" % (
        stamp(), A.template, A.build, cfg(A.template).get("template")))
    pve.wait_task(pve.api("DELETE", "/nodes/%s/qemu/%s?purge=1" % (node(), A.build)))
    print("[%s] %s destroyed" % (stamp(), A.build))


def step_verify():
    if exists(A.check):
        sys.exit("VM %s exists: refused" % A.check)
    t0 = time.time()
    clone(A.template, A.check, "fb-template-check")
    pve.api("POST", "/nodes/%s/qemu/%s/config" % (node(), A.check), address())
    pve.wait_task(pve.api("POST", "/nodes/%s/qemu/%s/status/start" % (node(), A.check), {}))
    up = wait_agent(A.check)
    st, secs = wait_cloud_init(A.check)
    _code, out, _err = sh(A.check, "echo upgradable=$(apt list --upgradable 2>/dev/null | grep -c upgradable); "
                                   "test -s /etc/machine-id && echo machine-id-set; "
                                   "echo host-keys=$(ls /etc/ssh/ssh_host_*_key 2>/dev/null | wc -l)")
    print("[%s] the check clone: agent %.0f s, cloud-init %s at +%.0f s (Proxmox's default upgrade on); %s" % (
        stamp(), up, st, secs, " | ".join((out or "").split())[:200]))
    pve.wait_task(pve.api("POST", "/nodes/%s/qemu/%s/status/stop" % (node(), A.check), {}))
    wait_stopped(A.check)
    pve.wait_task(pve.api("DELETE", "/nodes/%s/qemu/%s?purge=1" % (node(), A.check)))
    print("[%s] the check clone %s destroyed (%.0f s in all)" % (stamp(), A.check, time.time() - t0))


STEPS = (("build", step_build), ("upgrade", step_upgrade), ("prep", step_prep), ("templ", step_templ),
         ("swap", step_swap), ("verify", step_verify))


def main():
    global A
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--env", help="file with PVE_HOST/PVE_TOKEN/PVE_NODE (export lines), as lab/ops.py takes")
    p.add_argument("--pve-host", help="override PVE_HOST")
    p.add_argument("--template", default="9000", help="the template to rebuild (default 9000)")
    p.add_argument("--build", default="9001", help="the VM it is built in, a free id (default 9001)")
    p.add_argument("--check", default="105", help="the verify clone, a free id (default 105)")
    p.add_argument("--addr", default="10.10.10.60/24", help="a free address for BUILD and the check clone")
    p.add_argument("--gw", default="10.10.10.1", help="their gateway")
    p.add_argument("step", choices=[s for s, _ in STEPS] + ["all"])
    A = p.parse_args()
    load_env(A.env, A.pve_host)
    if A.step == "all":
        for _name, fn in STEPS:
            fn()
    else:
        dict(STEPS)[A.step]()


if __name__ == "__main__":
    main()
