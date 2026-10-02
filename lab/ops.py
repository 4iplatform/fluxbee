#!/usr/bin/env python3
"""ops.py — day-to-day operation of a Fluxbee deployment on Proxmox, on top of lab/pve.py.

Everything runs through the Proxmox guest agent (as root on the VMs), so it needs the same env as
pve.py (PVE_HOST, PVE_TOKEN, PVE_NODE). Load it from a file with --env, and override the host with
--pve-host when the file points at a tunnel you do not need (e.g. --pve-host 192.168.8.207).

    python3 lab/ops.py --env scratchpad/pve.env --pve-host 192.168.8.207 <command> ...

Commands
  admin METHOD PATH [JSON|@file]   call SY.admin on the motherbee (curl inside its VM; no shell,
                                   so query strings with '&' and JSON bodies pass untouched)
  run VM SCRIPT [ARGS...]          run a local bash or python (.py) script on a VM
  versions                         core version and manifest hash per hive
  opa-status [HIVE...]             user OPA policy per hive: version, in_sync, published, router
  health [--since HH:MM]           per VM: failed units, gate denials, routing.src drops (UTC)
  build VERSION [--wait]           pull and build the .deb on the build VM (log /root/build-V.log)
  publish VERSION                  publish the built .deb to the apt repo on the build VM
  deploy VERSION --snapshot NAME [--drop OLD]
                                   snapshot every VM (max 3 per VM: name the one to drop, which
                                   must be the oldest), apt install on the motherbee, wait for
                                   its admin, core-update every spoke, wait for all to report
                                   VERSION, then health

The inventory defaults to the 8.x PROD deployment (HANDBOOK §12): motherbee=100, worker1=101,
ingress1=102, egress1=103, build VM 110. Override with FLUXBEE_HIVES="motherbee=100,worker1=101"
and FLUXBEE_BUILD_VM=110.
"""
import argparse
import base64
import json
import os
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
MAX_SNAPSHOTS = 3
DEFAULT_HIVES = "motherbee=100,worker1=101,ingress1=102,egress1=103"
PRIMARY = "motherbee"
pve = None  # lab/pve.py, imported after the env is loaded (it reads PVE_* at import)


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


def hives():
    out = {}
    for pair in os.environ.get("FLUXBEE_HIVES", DEFAULT_HIVES).split(","):
        name, vm = pair.split("=")
        out[name.strip()] = vm.strip()
    return out


def build_vm():
    return os.environ.get("FLUXBEE_BUILD_VM", "110")


def vm_exec(vm, argv, quiet=False):
    """Run argv on the VM (as root, HOME set: FINDINGS B-7). Returns (code, out, err)."""
    code, out, err = pve._agent_exec(pve.node(), vm, ["/usr/bin/env", "HOME=/root"] + argv)
    if err and not quiet:
        sys.stderr.write(err if err.endswith("\n") else err + "\n")
    return code, out, err


def vm_bash(vm, script, quiet=False):
    return vm_exec(vm, ["/bin/bash", "-c", script], quiet)


# ---------------- admin ----------------

def admin(method, path, body=None, timeout=120):
    """SY.admin on the motherbee. Returns the parsed JSON (or {'raw': text})."""
    argv = ["curl", "-s", "-m", str(timeout), "-X", method, "127.0.0.1:8080" + path]
    if body is not None:
        argv += ["-H", "content-type:application/json", "-d", json.dumps(body)]
    _, out, _ = vm_exec(hives()[PRIMARY], argv, quiet=True)
    try:
        return json.loads(out)
    except ValueError:
        return {"raw": out}


def cmd_admin(a):
    body = None
    if a.body:
        body = json.load(open(a.body[1:])) if a.body.startswith("@") else json.loads(a.body)
    print(json.dumps(admin(a.method.upper(), a.path, body), indent=1))


# ---------------- run ----------------

def cmd_run(a):
    script = open(a.script, "rb").read()
    b64 = base64.b64encode(script).decode("ascii")
    runner = "python3 -" if a.script.endswith(".py") else "bash -s --"
    args = " ".join("'%s'" % x.replace("'", "'\\''") for x in a.args)
    code, out, _ = vm_bash(a.vm, "echo %s | base64 -d | %s %s" % (b64, runner, args))
    sys.stdout.write(out)
    sys.exit(code or 0)


# ---------------- versions / opa / health ----------------

def core_of(hive):
    reply = admin("GET", "/hives/%s/versions" % hive)
    core = ((reply.get("payload") or {}).get("hive") or {}).get("core") or {}
    versions = sorted({(c or {}).get("version") for c in (core.get("components") or {}).values()}, key=str)
    return core.get("status"), versions, core.get("manifest_hash") or ""


def cmd_versions(_a):
    for hive in hives():
        status, versions, digest = core_of(hive)
        print("%-10s core=%s versions=%s hash=%s" % (hive, status, versions, digest[:16]))


def cmd_opa_status(a):
    for hive in a.hives or list(hives()):
        reply = admin("GET", "/hives/%s/opa/status" % hive)
        entries = reply.get("responses") or []
        if not entries:
            print("%-10s %s %s" % (hive, reply.get("status"), reply.get("error_code") or reply.get("raw", "")[:80]))
            continue
        p = entries[0].get("payload") or {}
        routers = ", ".join("v%s %s" % (r.get("policy_version"), r.get("status")) for r in p.get("routers") or [])
        line = "%-10s v%s in_sync=%s pub=%s region=%s routers=[%s]" % (
            hive, p.get("current_version"), p.get("in_sync"), (p.get("published_hash") or "-")[:16],
            (p.get("region_hash") or "-")[:16], routers)
        if p.get("waiting"):
            line += " WAITING=%s" % p["waiting"]
        if p.get("last_error"):
            line += " last_error=%s" % p["last_error"]
        print(line)


# Gate denials and routing.src drops are logged by the router only: reading its unit alone also
# keeps out the guest agent's own log of this command (which contains the patterns).
# A journal that cannot be read is reported, never counted as zero.
HEALTH_SCRIPT = r"""
if ! log=$(journalctl -u rt-gateway --since "%s" --no-pager -o cat 2>&1); then
  echo "journal unreadable: $(printf '%%s' "$log" | head -1)"; exit 1
fi
echo "failed=$(systemctl --failed --no-legend --plain | wc -l)" \
     "denials=$(printf '%%s\n' "$log" | grep -c 'dropped protected SYSTEM action')" \
     "srcdrops=$(printf '%%s\n' "$log" | grep -c 'routing.src is not the sending node')"
"""


def health(since):
    rows, threads = {}, []

    def one(hive, vm):
        _, out, _ = vm_bash(vm, HEALTH_SCRIPT % since, quiet=True)
        rows[hive] = out.strip()

    for hive, vm in hives().items():
        t = threading.Thread(target=one, args=(hive, vm))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()
    ok = True
    for hive in hives():
        print("%-10s %s" % (hive, rows.get(hive, "no answer")))
        ok = ok and rows.get(hive, "").startswith("failed=0 denials=0 srcdrops=0")
    return ok


def utc_stamp(epoch):
    # journalctl reads a bare time in the VM's own zone; the suffix makes it UTC everywhere.
    return time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime(epoch))


def cmd_health(a):
    if a.since:
        since = "%s %s:00 UTC" % (time.strftime("%Y-%m-%d", time.gmtime()), a.since)
    else:
        since = utc_stamp(time.time() - 3600)
    sys.exit(0 if health(since) else 1)


# ---------------- build / publish ----------------

def cmd_build(a):
    v = a.version
    log = "/root/build-%s.log" % v
    # The pull runs in the foreground: a failed pull stops here instead of leaving no log behind
    # (with `pull && nohup ... &` the whole list went to the background, pull included).
    script = (
        "set -e; cd /opt/fluxbee; sudo -u fluxops git pull --ff-only -q; "
        "nohup bash -c 'export PATH=/root/.cargo/bin:$PATH; cd /opt/fluxbee; "
        "echo \"=== BUILD START $(date -Is) commit $(git rev-parse --short HEAD)\"; "
        "bash packaging/build-deb.sh %s; echo \"=== BUILD END rc=$? $(date -Is)\"' "
        "> %s 2>&1 < /dev/null & sleep 2; head -1 %s" % (v, log, log))
    code, out, _ = vm_bash(build_vm(), script)
    print(out.strip())
    if code:
        sys.exit("the build did not start (git pull or launch failed)")
    if not a.wait:
        return
    while True:
        _, out, _ = vm_bash(build_vm(), "test -f %s || echo NO-LOG; grep '=== BUILD END' %s || true"
                            % (log, log), quiet=True)
        if "NO-LOG" in out:
            sys.exit("%s is gone: the build is not running" % log)
        if out.strip():
            print(out.strip())
            sys.exit(0 if "rc=0" in out else 1)
        time.sleep(15)


def cmd_publish(a):
    deb = "/opt/fluxbee/dist/fluxbee_%s_amd64.deb" % a.version
    script = ("t0=$(date +%%s); cd /opt/fluxbee && bash scripts/apt-repo-publish.sh --deb %s "
              "> /root/publish-%s.log 2>&1; rc=$?; tail -1 /root/publish-%s.log; "
              "echo \"publish rc=$rc in $(( $(date +%%s) - t0 )) s\"; exit $rc" % (deb, a.version, a.version))
    code, out, _ = vm_bash(build_vm(), script)
    print(out.strip())
    sys.exit(code or 0)


# ---------------- deploy ----------------

def snapshot_all(new, drop):
    n = pve.node()
    plans = {}
    for hive, vm in hives().items():
        have = pve._snapshots_by_age(n, vm)
        if len(have) >= MAX_SNAPSHOTS:
            oldest = have[0]["name"]
            if drop != oldest:
                sys.exit("vm%s (%s) has %d snapshots: name the oldest to drop: --drop %s"
                         % (vm, hive, len(have), oldest))
            plans[vm] = oldest
    for hive, vm in hives().items():
        if vm in plans:
            pve.wait_task(pve.api("DELETE", "/nodes/%s/qemu/%s/snapshot/%s" % (n, vm, plans[vm])))
            print("  vm%s: dropped %s" % (vm, plans[vm]))
        pve.wait_task(pve.api("POST", "/nodes/%s/qemu/%s/snapshot" % (n, vm), {"snapname": new, "vmstate": 0}))
        print("  vm%s: snapshot %s" % (vm, new))


def wait_for(what, check, timeout, every=6):
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = check()
        if result:
            return result
        time.sleep(every)
    sys.exit("timed out after %d s waiting for %s" % (timeout, what))


def cmd_deploy(a):
    v = a.version
    started = time.time()
    since = utc_stamp(started)
    print("== snapshots")
    snapshot_all(a.snapshot, a.drop)

    print("== apt install fluxbee=%s on %s" % (v, PRIMARY))
    code, out, _ = vm_bash(hives()[PRIMARY], (
        "apt-get update -qq >/dev/null 2>&1; DEBIAN_FRONTEND=noninteractive "
        "apt-get install -y -qq fluxbee=%s >/dev/null 2>&1; dpkg-query -W fluxbee" % v))
    print("  " + out.strip())
    if code or v not in out:
        sys.exit("install failed")

    print("== waiting for the motherbee admin on %s" % v)

    def primary_ready():
        status, versions, digest = core_of(PRIMARY)
        return digest if versions == [v] else None

    digest = wait_for("the motherbee core on %s" % v, primary_ready, 300)
    print("  manifest %s" % digest[:16])

    spokes = [h for h in hives() if h != PRIMARY]
    print("== core update: %s" % ", ".join(spokes))
    # The call answers TIMEOUT even when the update works (PENDING-BUGS U-8b): fire them all at
    # once and judge by what each spoke reports, not by the answer.
    threads = [threading.Thread(target=admin, args=(
        "POST", "/hives/%s/update" % h, {"category": "core", "manifest_version": 0, "manifest_hash": digest}))
        for h in spokes]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    for h in spokes:
        def spoke_ready(h=h):
            status, versions, got = core_of(h)
            return got if versions == [v] and got == digest else None
        wait_for("%s on %s" % (h, v), spoke_ready, 600)
        print("  %s on %s" % (h, v))

    print("== health since %s" % since)
    ok = health(since)
    print("deploy of %s done in %d s" % (v, time.time() - started))
    sys.exit(0 if ok else 1)


def main():
    p = argparse.ArgumentParser(prog="ops", description=__doc__.split("\n")[0])
    p.add_argument("--env", help="file with PVE_HOST/PVE_TOKEN/PVE_NODE (export lines)")
    p.add_argument("--pve-host", help="override PVE_HOST")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("admin"); s.add_argument("method"); s.add_argument("path"); s.add_argument("body", nargs="?")
    s.set_defaults(fn=cmd_admin)
    s = sub.add_parser("run"); s.add_argument("vm"); s.add_argument("script"); s.add_argument("args", nargs="*")
    s.set_defaults(fn=cmd_run)
    sub.add_parser("versions").set_defaults(fn=cmd_versions)
    s = sub.add_parser("opa-status"); s.add_argument("hives", nargs="*"); s.set_defaults(fn=cmd_opa_status)
    s = sub.add_parser("health"); s.add_argument("--since", help="HH:MM (UTC), default one hour ago")
    s.set_defaults(fn=cmd_health)
    s = sub.add_parser("build"); s.add_argument("version"); s.add_argument("--wait", action="store_true")
    s.set_defaults(fn=cmd_build)
    s = sub.add_parser("publish"); s.add_argument("version"); s.set_defaults(fn=cmd_publish)
    s = sub.add_parser("deploy"); s.add_argument("version")
    s.add_argument("--snapshot", required=True, help="name of the snapshot taken on every VM first")
    s.add_argument("--drop", help="the oldest snapshot to delete where a VM already has 3")
    s.set_defaults(fn=cmd_deploy)
    a = p.parse_args()
    # Progress shows as it happens even when the output goes to a pipe or a file.
    sys.stdout.reconfigure(line_buffering=True)
    load_env(a.env, a.pve_host)
    a.fn(a)


if __name__ == "__main__":
    main()
