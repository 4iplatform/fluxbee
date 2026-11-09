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
  health [--since HH:MM]           per VM: failed units, gate denials, routing.src drops, records
                                   the router stream lost, usage records lost or rejected (UTC);
                                   then the edge certificate (edge-cert) and the web door (web)
  build VERSION [--wait]           pull and build the .deb on the build VM (log /root/build-V.log)
  publish VERSION                  publish the built .deb to the apt repo on the build VM; the
                                   repo keeps the newest 10 versions, dist/ drops the older .debs
  deploy VERSION --snapshot NAME [--drop OLD]
                                   snapshot every VM (one per VM: the new one replaces the one
                                   named in --drop), apt install on the motherbee, wait for
                                   its admin and for the spokes to answer, then SY.admin's
                                   installation update (POST /installation/update, from
                                   0.1.123), followed until every hive is on the release, each
                                   hive's transitions as they happen; below 0.1.123 a core
                                   update per spoke; then health. Each step carries its time,
                                   and a summary of the phases ends it
  timeline --since 'YYYY-MM-DD HH:MM:SS' [--until ...] [--vm NAME=VMID ...] [--all]
                                   one timeline, in time order, of every hive's core units'
                                   journals (UTC): the milestones only (starts, stops,
                                   connections, updates, joins, settles, warnings), or every
                                   line with --all; each hive's clock offset first

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
MAX_SNAPSHOTS = 1
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
    """(status, versions, hash, source) of a hive's core. The hash is what the hive INSTALLED when
    it says so, else what dist delivered: the rule of /versions' own comparison (X1). On a spoke
    `core.manifest_hash` and the component versions follow dist, which Syncthing updates before
    any core update runs, so judging by them reported spokes as updated that were not (0.1.56).
    An install it cannot name reads as "" (unknown), never as the dist hash."""
    reply = admin("GET", "/hives/%s/versions" % hive)
    core = ((reply.get("payload") or {}).get("hive") or {}).get("core") or {}
    versions = sorted({(c or {}).get("version") for c in (core.get("components") or {}).values()}, key=str)
    installed = core.get("installed")
    if installed:
        return core.get("status"), versions, installed.get("manifest_hash") or "", "installed"
    return core.get("status"), versions, core.get("manifest_hash") or "", "dist"


def update_open_items(hive, expect_settle):
    """What the core update a spoke last ran leaves open, or None while it is not over yet. Since
    0.1.117 the orchestrator a core update installed finishes it at its start
    (hive-config-update-v1 §3): the hash the previous orchestrator recorded says the binaries are
    in, not that the release's list is. Open: components left out, a settle with something
    pending or an error, and `core.awaiting_update`."""
    reply = admin("GET", "/hives/%s/versions" % hive)
    core = ((reply.get("payload") or {}).get("hive") or {}).get("core") or {}
    last = core.get("last_update") or {}
    settled = last.get("settled")
    if expect_settle and "sy-orchestrator" in (last.get("updated") or []) and not settled:
        return None
    items = []
    if last.get("left_out"):
        items.append("left_out=%s" % ",".join(last["left_out"]))
    if settled and not settled.get("settled"):
        items.append("settle pending=%s errors=%s" % (settled.get("pending"), settled.get("errors")))
    awaiting = core.get("awaiting_update")
    if awaiting:
        items.append("awaiting_update=%s" % ",".join(
            c.get("component", "?") for c in awaiting.get("components") or []))
    return items


def spoke_update_times(hive, since):
    """A spoke's last core update by its own records, in seconds after `since` (epoch): its
    restarts' start and end (`core-update-last.json`), its install (`core-installed.json`) and the
    new orchestrator's settle. Each on the spoke's clock."""
    reply = admin("GET", "/hives/%s/versions" % hive)
    core = ((reply.get("payload") or {}).get("hive") or {}).get("core") or {}
    last = core.get("last_update") or {}
    stamps = [
        ("restarts began", last.get("started_at_ms")),
        ("ended", last.get("finished_at_ms")),
        ("installed", (core.get("installed") or {}).get("installed_at_ms")),
        ("settled", (last.get("settled") or {}).get("at_ms")),
    ]
    parts = []
    for name, ms in stamps:
        try:
            parts.append("%s %+.1fs" % (name, int(ms) / 1000 - since))
        except (TypeError, ValueError):
            parts.append("%s -" % name)
    return ", ".join(parts)


def cmd_versions(_a):
    for hive in hives():
        status, versions, digest, source = core_of(hive)
        print("%-10s core=%s versions=%s hash=%s (%s)" % (hive, status, versions, digest[:16], source))


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


# Gate denials, routing.src drops and the stream's losses are logged by the router only: reading
# its unit alone also keeps out the guest agent's own log of this command (which contains the
# patterns). The stream logs at most one line per 30 s with its losses by reason ("expired: 12");
# `streamlost` sums them. The usage records among them come again in a line of their own,
# `usagelost` (A-70 stage 3). SY.storage (the motherbee only) logs at most one line per 30 s with
# the usage records it refused, stored under another tenant than they claimed, or could not verify;
# `usagerej` sums them. A journal that cannot be read is reported, never counted as zero.
HEALTH_SCRIPT = r"""
if ! log=$(journalctl -u rt-gateway --since "%s" --no-pager -o cat 2>&1); then
  echo "journal unreadable: $(printf '%%s' "$log" | head -1)"; exit 1
fi
if ! storage=$(journalctl -u sy-storage --since "%s" --no-pager -o cat 2>&1); then
  echo "journal unreadable: $(printf '%%s' "$storage" | head -1)"; exit 1
fi
echo "failed=$(systemctl --failed --no-legend --plain | wc -l)" \
     "denials=$(printf '%%s\n' "$log" | grep -c 'dropped protected SYSTEM action')" \
     "srcdrops=$(printf '%%s\n' "$log" | grep -c 'routing.src is not the sending node')" \
     "streamlost=$(printf '%%s\n' "$log" | grep 'router stream lost records' \
                   | sed 's/.*router stream lost records in the last [0-9]* s//' \
                   | grep -oE ': [0-9]+' | awk '{s+=$2} END {print s+0}')" \
     "usagelost=$(printf '%%s\n' "$log" | grep 'router stream lost usage records' \
                  | sed 's/.*router stream lost usage records in the last [0-9]* s//' \
                  | grep -oE ': [0-9]+' | awk '{s+=$2} END {print s+0}')" \
     "usagerej=$(printf '%%s\n' "$storage" \
                 | grep 'usage records refused, mismatched or unverified' \
                 | grep -oE '(refused|mismatched|unverified)=[0-9]+' | cut -d= -f2 \
                 | awk '{s+=$1} END {print s+0}')"
"""


def health(since):
    rows, threads = {}, []

    def one(hive, vm):
        _, out, _ = vm_bash(vm, HEALTH_SCRIPT % (since, since), quiet=True)
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
        ok = ok and rows.get(hive, "") == (
            "failed=0 denials=0 srcdrops=0 streamlost=0 usagelost=0 usagerej=0")
    certificate_ok = certificate_health()
    web_health()
    return certificate_ok and ok


def certificate_health():
    """The edge certificate's open conditions (GET /acme/status, docs/acme-edge-certificates-v1.md
    §7): a critical one fails the health check. A hive without the route (before 0.1.82) passes."""
    status = admin("GET", "/acme/status", timeout=30)
    body = status.get("payload") if isinstance(status.get("payload"), dict) else status
    if "conditions" not in body:
        return True
    conditions = body.get("conditions") or []
    critical = [c.get("kind") for c in conditions if c.get("severity") == "critical"]
    config = body.get("config") or {}
    # `names` since 0.1.84; an SY.admin before it (or rolled back) lists its one `name`.
    names = config.get("names") or [n for n in [config.get("name")] if n]
    print("%-10s acme=%s names=%s conditions=%s%s" % (
        "edge-cert", config.get("state"), ",".join(names) or "none",
        ",".join(c.get("kind", "?") for c in conditions) or "none",
        " CRITICAL" if critical else ""))
    return not critical


def web_health():
    """The web door (GET /web/status, docs/io-web-v2.md §11.2): the declared host, the edge's
    answer to the web routes, the edge's way to io.web (`unknown` unless the last push was
    acknowledged) with the kind and time of its last failure, and io.web's state with its health
    when it is not HEALTHY. It never fails the health check: io.web runs only once the operator
    starts it, and the edge answers again within a minute of a restart."""
    reply = admin("GET", "/web/status", timeout=15)
    body = reply.get("payload")
    if not isinstance(body, dict) or "io_web" not in body:
        print("%-10s %s" % ("web", reply.get("error_code") or reply.get("raw", "")[:60] or "no answer"))
        return
    web, io_web = body.get("web") or {}, body.get("io_web") or {}
    routes = body.get("routes") or {}
    host = ",".join(web.get("hosts") or []) or ("none" if web.get("state") == "absent" else web.get("state"))
    upstream = routes.get("upstream") or {}
    failure = upstream.get("last_failure") or {}
    health = (io_web.get("answer") or {}).get("health_state")
    print("%-10s host=%s route=%s upstream=%s%s io.web=%s%s" % (
        "web", host, routes.get("state"), upstream.get("state") or "unknown",
        "(%s@%s)" % (failure.get("kind"), failure.get("at")) if failure else "",
        io_web.get("state"), "(%s)" % health if health and health != "HEALTHY" else ""))


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


# Versions the apt repo keeps: `apt install fluxbee=<v>` reaches that far back (a VM's one
# snapshot undoes only the last release).
REPO_KEEP = 10


def cmd_publish(a):
    deb = "dist/fluxbee_%s_amd64.deb" % a.version
    log = "/root/publish-%s.log" % a.version
    # The repo keeps the newest REPO_KEEP versions, and dist/ drops every .deb older than this one,
    # which the repo now holds: each .deb was stored twice, forever (FINDINGS B-17). Both print
    # what they remove.
    prune_dist = ("for f in dist/fluxbee_*_amd64.deb; do [ -e \"$f\" ] && [ \"$f\" != %s ] && "
                  "[ \"$(printf '%%s\\n' \"$f\" %s | sort -V | head -1)\" = \"$f\" ] && "
                  "rm -f \"$f\" && echo \"   dist: removed ${f#dist/}\"; done" % (deb, deb))
    script = ("t0=$(date +%%s); cd /opt/fluxbee && bash scripts/apt-repo-publish.sh --deb %s --keep %d "
              "> %s 2>&1; rc=$?; grep '^   pruned ' %s; tail -1 %s; "
              "if [ $rc -eq 0 ]; then %s; fi; "
              "echo \"publish rc=$rc in $(( $(date +%%s) - t0 )) s\"; exit $rc"
              % (deb, REPO_KEEP, log, log, log, prune_dist))
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
    # The new snapshot first, then the old one goes: a VM is never left without its backup.
    for hive, vm in hives().items():
        pve.wait_task(pve.api("POST", "/nodes/%s/qemu/%s/snapshot" % (n, vm), {"snapname": new, "vmstate": 0}))
        print("  vm%s: snapshot %s" % (vm, new))
        if vm in plans:
            pve.wait_task(pve.api("DELETE", "/nodes/%s/qemu/%s/snapshot/%s" % (n, vm, plans[vm])))
            print("  vm%s: dropped %s" % (vm, plans[vm]))


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
    phases = []  # (the phase that just ended, seconds since the start)

    def mark(text, ended=None):
        """Print `text` with the seconds since the deploy started, and close the phase `ended`."""
        elapsed = time.time() - started
        print("[%6.1fs] %s" % (elapsed, text))
        if ended:
            phases.append((ended, elapsed))

    mark("== snapshots")
    snapshot_all(a.snapshot, a.drop)

    mark("== apt install fluxbee=%s on %s" % (v, PRIMARY), "snapshots")
    # The seed's NOTE lines say which base runtime keeps a hand-published current (FINDINGS A-122).
    code, out, _ = vm_bash(hives()[PRIMARY], (
        "apt-get update -qq >/dev/null 2>&1; DEBIAN_FRONTEND=noninteractive "
        "apt-get install -y -qq fluxbee=%s >/tmp/fluxbee-install.log 2>&1; "
        "grep -h 'NOTE: base runtime' /tmp/fluxbee-install.log | sed 's/^ *//'; "
        "dpkg-query -W fluxbee" % v))
    for line in out.strip().splitlines():
        print("  " + line)
    if code or v not in out:
        sys.exit("install failed")

    mark("== waiting for the motherbee admin on %s" % v, "apt install on the motherbee")

    def primary_ready():
        status, versions, digest, _ = core_of(PRIMARY)
        return digest if versions == [v] else None

    digest = wait_for("the motherbee core on %s" % v, primary_ready, 300)
    mark("  manifest %s" % digest[:16], "the motherbee's admin on the version")

    spokes = [h for h in hives() if h != PRIMARY]
    spoke_times = {h: {} for h in spokes}
    # The motherbee's admin answers before its router has the spokes back: an update sent then
    # fails with TRANSPORT_ERROR (0.1.56). Wait until each spoke's orchestrator answers.
    for h in spokes:
        wait_for("%s answering" % h, lambda h=h: core_of(h)[0] == "ok" or None, 300)
    # From 0.1.123 SY.admin's installation update brings every hive to the motherbee's release,
    # core then config (hive-config-update-v1 §6, stage 4). Deploying an older release (the
    # rollback below 0.1.123) keeps the per-spoke core updates, which its SY.admin knows.
    if tuple(int(x) for x in v.split(".")[:3]) >= INSTALLATION_UPDATE_SINCE:
        mark("== installation update: %s" % ", ".join(hives()), "the spokes answering the motherbee")
        update_sent = time.time()
        settled_ok = installation_update(v, mark, spoke_times, phases, started)
    else:
        mark("== core update: %s" % ", ".join(spokes), "the spokes answering the motherbee")
        update_sent = time.time()
        settled_ok = spoke_core_updates(v, digest, spokes, mark, spoke_times, phases, started)

    mark("== health since %s" % since)
    ok = health(since) and settled_ok
    phases.append(("health", time.time() - started))

    # Where the deploy spent its time. A hive's times count from the update calls, and are when
    # this script saw each state, at most one poll late.
    print("== phases")
    previous = 0.0
    for name, ended in phases:
        print("  %-38s %6.1fs" % (name, ended - previous))
        previous = ended
    for h in sorted(spoke_times):
        times = spoke_times[h]
        seen = [name for name in TRANSITIONS if name in times]
        print("  %-10s %s after the update call" % (
            h, ", ".join("%s %5.1fs" % (name, times[name]) for name in seen) or "no transition seen"))
    # The spokes' own records, on their clocks: when the script saw a state can lag it, as the
    # motherbee's /versions forward is slow while a spoke's orchestrator restarts (0.1.120).
    for h in spokes:
        print("  %-10s by its own records: %s" % (h, spoke_update_times(h, update_sent)))
    print("deploy of %s done in %d s" % (v, time.time() - started))
    sys.exit(0 if ok else 1)


# The first release whose SY.admin runs the installation's update (hive-config-update-v1 §6).
INSTALLATION_UPDATE_SINCE = (0, 1, 123)
# How long the call waits: SY.admin's ceiling for update_installation, 390 s, and a margin. It
# answers once every core answered (`restarting`), about 20 s on 8.x.
INSTALLATION_UPDATE_CALL_TIMEOUT = 420
# How long the deploy follows the update, until every hive is on the target, or left out.
INSTALLATION_UPDATE_WAIT = 900
# The transitions a deploy prints for each hive, in order.
TRANSITIONS = ("answered", "installed", "settled", "config applied")


def update_transitions(hive, entry):
    """The transitions a hive's record in the installation's update shows: its core update
    answered, the target installed, its update settled, its config applied. Each with the ms its
    record gives after the attempt started (None when it gives none). The motherbee is the core's
    reference: only its config."""
    core = entry.get("core") or {}
    config = entry.get("config") or {}
    outcome = core.get("outcome") or {}
    seen = {}
    if hive != PRIMARY:
        for name, key in (("answered", "answered_ms"), ("installed", "installed_ms")):
            if outcome.get(key) is not None:
                seen[name] = outcome[key]
        if core.get("state") == "on_target":
            seen["settled"] = outcome.get("settled_ms")
    if config.get("state") == "on_target":
        seen["config applied"] = (config.get("outcome") or {}).get("elapsed_ms")
    return seen


def hive_moving(entry):
    """Whether something still moves a hive: a core restarting or settling, a config being
    applied, or a state SY.admin has not read yet. A pending hive (an error, sync_pending at its
    bound, no answer) and an excluded one wait for the operator, or for the automatic step."""
    return any((entry.get(category) or {}).get("state") in ("in_flight", "unknown")
               for category in ("core", "config"))


def hive_left_as_is(hive, entry):
    """A line for each category of a hive the update leaves as it is now: excluded, or pending
    with what its last attempt left, and what moves it."""
    lines = []
    for category in ("core", "config"):
        part = entry.get(category) or {}
        state = part.get("state")
        if state not in ("excluded", "pending"):
            continue
        outcome = part.get("outcome") or {}
        status = outcome.get("status")
        if state == "pending" and status not in ("error", "sync_pending", "not_answered"):
            continue
        lines.append("%s %s %s%s%s: %s" % (
            hive, category, state, " (%s)" % status if status and state == "pending" else "",
            " %s" % outcome["error_code"] if outcome.get("error_code") else "",
            part.get("reason") or outcome.get("detail") or ""))
        if part.get("next"):
            lines.append("    next: %s" % part["next"])
    return lines


def installation_update(v, mark, spoke_times, phases, started):
    """The installation's update (hive-config-update-v1 §6, stage 4): one call to SY.admin, which
    brings every hive to the motherbee's core and installation file; then its record, read every
    3 s, until the update is done, or until nothing moves a hive any more (each left out is named,
    with what moves it), or the wait ends. Each hive's transitions print as they happen, with the
    seconds since the call. True when every hive is on the target."""
    call_at = time.time()
    call_at_ms = int(call_at * 1000)
    replies = {}

    def call():
        replies["post"] = admin("POST", "/installation/update", {"reason": "deploy %s" % v},
                                timeout=INSTALLATION_UPDATE_CALL_TIMEOUT)
        replies["answered_at"] = time.time()

    caller = threading.Thread(target=call)
    caller.start()
    seen, reported, quiet, update, printed_post, ours = set(), set(), 0, {}, False, None
    deadline = call_at + INSTALLATION_UPDATE_WAIT
    while True:
        time.sleep(3)
        if "post" in replies and not printed_post:
            printed_post = True
            post = replies["post"]
            payload = post.get("payload") or {}
            ours = (payload.get("update") or {}).get("id")
            mark("  update_installation answered in %.1fs: status=%s%s%s" % (
                replies["answered_at"] - call_at,
                post.get("status") or post.get("raw", "")[:60],
                " error_code=%s" % post["error_code"] if post.get("error_code") else "",
                " update=%s" % (payload.get("update") or {}).get("id") if payload.get("update") else ""),
                "the installation update call")
            for line in (payload.get("message"), post.get("error_detail")):
                if line:
                    print("    %s" % line)
            if post.get("status") == "error" or payload.get("nothing_to_do"):
                break
        reply = admin("GET", "/installation/update", timeout=30)
        current = (reply.get("payload") or {}).get("update") or {}
        # Only this call's update: the one its answer names, the open one, or one closed since the
        # call (the motherbee's clock, a few seconds of offset allowed).
        if (current.get("id") and current.get("id") == ours) or current.get("state") == "open" \
                or (current.get("closed_at_ms") or 0) >= call_at_ms - 5000:
            update = current
        for hive, entry in sorted((update.get("hives") or {}).items()):
            for name, ms in update_transitions(hive, entry).items():
                if (hive, name) in seen:
                    continue
                seen.add((hive, name))
                spoke_times.setdefault(hive, {})[name] = time.time() - call_at
                mark("  %s %s at +%.1fs%s" % (
                    hive, name, time.time() - call_at,
                    " (its record: %.1fs)" % (ms / 1000.0) if isinstance(ms, (int, float)) else ""))
            for line in hive_left_as_is(hive, entry):
                if (hive, line) not in reported:
                    reported.add((hive, line))
                    print("  " + line)
        if update.get("state") == "done":
            mark("  update %s done: every hive on the target" % update.get("id"))
            break
        if not caller.is_alive():
            moving = any(hive_moving(entry) for entry in (update.get("hives") or {}).values())
            # Twice in a row: a config goes in flight right after its core settles.
            quiet = 0 if moving else quiet + 1
            if quiet >= 2:
                mark("  update %s left open: %s" % (
                    update.get("id"), "; ".join(update.get("pending") or []) or "see GET /installation/update"))
                break
        if time.time() > deadline:
            mark("  the update was followed for %d s: GET /installation/update shows the rest"
                 % INSTALLATION_UPDATE_WAIT)
            break
    caller.join()
    phases.append(("the hives on the target", time.time() - started))
    post = replies.get("post") or {}
    if (post.get("payload") or {}).get("nothing_to_do"):
        return True
    return update.get("state") == "done"


def spoke_core_updates(v, digest, spokes, mark, spoke_times, phases, started):
    """The core update of every spoke, one call each (the deploy before 0.1.123, whose SY.admin has
    no installation update): then each spoke on the version, then settled. True when every spoke
    settled with nothing open."""
    # Since U-8b the call answers once the spoke has the new binaries in place (phase
    # `restarting`); the restarts run after it. Fire them all at once, retry one that did not
    # get through, print each answer, and still judge each spoke by what it installed.
    replies = {}
    update_sent = time.time()

    def update(h):
        for attempt in range(3):
            if attempt:
                time.sleep(10)
            replies[h] = admin("POST", "/hives/%s/update" % h,
                               {"category": "core", "manifest_version": 0, "manifest_hash": digest})
            spoke_times[h]["answered"] = time.time() - update_sent
            r = replies[h]
            if (r.get("status") or (r.get("payload") or {}).get("status")) == "ok":
                return

    threads = [threading.Thread(target=update, args=(h,)) for h in spokes]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    for h in spokes:
        r = replies.get(h) or {}
        p = r.get("payload") or {}
        print("  %s answered in %.1fs: status=%s%s%s" % (
            h, spoke_times[h].get("answered", 0),
            r.get("status") or p.get("status") or r.get("raw", "")[:60],
            " error_code=%s" % (r.get("error_code") or p.get("error_code")) if (r.get("error_code") or p.get("error_code")) else "",
            " phase=%s" % p.get("phase") if p.get("phase") else ""))
    mark("  every update call answered", "the update calls")
    for h in spokes:
        def spoke_ready(h=h):
            status, versions, got, _ = core_of(h)
            return got if versions == [v] and got == digest else None
        wait_for("%s on %s" % (h, v), spoke_ready, 600)
        spoke_times[h]["installed"] = time.time() - update_sent
        mark("  %s on %s" % (h, v))
    phases.append(("the spokes installed", time.time() - started))

    # Each spoke's update over: the new orchestrator settled it, nothing left out or awaiting. A
    # left-out or awaiting binary clears once Syncthing delivers, so it gets two minutes.
    mark("== core update settled")
    expect_settle = tuple(int(x) for x in v.split(".")[:3]) >= (0, 1, 117)
    settled_ok = True
    for h in spokes:
        deadline = time.time() + 120
        items = update_open_items(h, expect_settle)
        while (items is None or items) and time.time() < deadline:
            time.sleep(6)
            items = update_open_items(h, expect_settle)
        if items is None:
            items = ["the new orchestrator did not settle the update"]
        settled_ok &= not items
        spoke_times[h]["settled"] = time.time() - update_sent
        mark("  %s %s" % (h, "; ".join(items) if items else "settled"))
    phases.append(("the spokes settled", time.time() - started))
    return settled_ok


# The units a timeline reads on each hive, and what counts as a milestone among their lines.
TIMELINE_UNITS = [
    "rt-gateway", "sy-orchestrator", "sy-admin", "sy-identity", "sy-vault", "sy-config-routes",
    "sy-opa-rules", "sy-cognition", "sy-timer", "sy-wf-rules", "sy-edge", "sy-storage",
    "sy-architect", "sy-frontdesk-gov", "fluxbee-syncthing",
]
TIMELINE_MILESTONES = (
    r"\b(Started|Stopped|Stopping|Starting|Failed|Main process exited)\b|connected to router|"
    r"hive ready|lifecycle node|service active|ADD_HIVE|add_hive|finalize|join|bootstrap|"
    r"core update|SYSTEM_UPDATE|update category|settle|revert|receive-only|awaiting_update|"
    r"installation update|automatic step|"
    r"dist check|peer|restart|WAN|uplink|(^|\s)(WARN|ERROR)\s"
)

# Runs on each VM: the units' journal lines in the window (UTC), as "epoch_us<TAB>unit<TAB>message",
# the tracing timestamp and ANSI codes dropped, milestones only unless asked for every line. The
# journal is read as a stream, and at most `limit` lines come back, the cut said in a last line.
TIMELINE_SCRIPT = r"""
import json, re, subprocess, sys
since, until, every, units, pattern, width, limit = sys.argv[1:8]
args = ["journalctl", "--no-pager", "-o", "json", "--since", since + " UTC"]
if until:
    args += ["--until", until + " UTC"]
for unit in units.split(","):
    args += ["-u", unit]
milestone = re.compile(pattern)
ansi = re.compile(r"\x1b\[[0-9;]*m")
stamp = re.compile(r"^\d{4}-\d\d-\d\dT[\d:.]+Z\s+")
shown, cut = 0, 0
proc = subprocess.Popen(args, stdout=subprocess.PIPE, text=True, errors="replace")
for line in proc.stdout:
    try:
        rec = json.loads(line)
    except ValueError:
        continue
    msg = rec.get("MESSAGE")
    if isinstance(msg, list):
        msg = bytes(msg).decode("utf-8", "replace")
    msg = stamp.sub("", ansi.sub("", msg or "")).replace("\t", " ").replace("\n", " ")
    if every != "1" and not milestone.search(msg):
        continue
    # systemd's own lines name the unit they are about in UNIT; a service's lines, in _SYSTEMD_UNIT.
    unit = (rec.get("UNIT") or rec.get("_SYSTEMD_UNIT") or rec.get("SYSLOG_IDENTIFIER") or "?")
    if shown >= int(limit):
        cut += 1
        continue
    shown += 1
    print("%s\t%s\t%s" % (rec.get("__REALTIME_TIMESTAMP", "0"), unit.replace(".service", ""), msg[: int(width)]))
proc.wait()
if cut:
    print("CUT\t%d lines past the first %s" % (cut, limit))
"""


def clock_offset(vm):
    """This VM's clock minus this machine's, in seconds, and the round trip it was read in."""
    before = time.time()
    _, out, _ = vm_bash(vm, "date +%s.%N", quiet=True)
    after = time.time()
    try:
        return float(out.strip()) - (before + after) / 2, after - before
    except ValueError:
        return None, after - before


def cmd_timeline(a):
    targets = dict(hives())
    for extra in a.vm or []:
        name, vm = extra.split("=")
        targets[name.strip()] = vm.strip()
    b64 = base64.b64encode(TIMELINE_SCRIPT.encode()).decode("ascii")
    rows, offsets, cuts, threads = [], {}, {}, []

    def one(hive, vm):
        offsets[hive] = clock_offset(vm)
        argv = [a.since, a.until or "", "1" if a.all else "0", ",".join(TIMELINE_UNITS),
                TIMELINE_MILESTONES, str(a.width), str(a.max_lines)]
        quoted = " ".join("'%s'" % x.replace("'", "'\\''") for x in argv)
        _, out, _ = vm_bash(vm, "echo %s | base64 -d | python3 - %s" % (b64, quoted), quiet=True)
        for line in out.splitlines():
            parts = line.split("\t", 2)
            if len(parts) == 3 and parts[0].isdigit():
                rows.append((int(parts[0]) / 1e6, hive, parts[1], parts[2]))
            elif parts[0] == "CUT":
                cuts[hive] = parts[1]

    for hive, vm in targets.items():
        t = threading.Thread(target=one, args=(hive, vm))
        t.start()
        threads.append(t)
    for t in threads:
        t.join()
    print("clock offsets against this machine (a guest-agent call each, so +-half its round trip):")
    for hive in targets:
        offset, rtt = offsets.get(hive, (None, 0))
        print("  %-10s %s (round trip %.2fs)" % (
            hive, "%+.3fs" % offset if offset is not None else "unknown", rtt))
    for hive, note in cuts.items():
        print("  %-10s CUT: %s (narrow the window, or raise --max-lines)" % (hive, note))
    if a.align:
        rows = [(ts - (offsets.get(hive, (None, 0))[0] or 0), hive, unit, msg)
                for ts, hive, unit, msg in rows]
    rows.sort()
    if not rows:
        print("no lines in the window")
        return
    first = rows[0][0]
    previous = first
    print("%9s %8s  %-10s %-18s %s" % ("t", "delta", "hive", "unit", "line (UTC %s)" % utc_stamp(first)))
    for ts, hive, unit, msg in rows:
        print("%8.3fs %7.3fs  %-10s %-18s %s" % (ts - first, ts - previous, hive, unit[:18], msg))
        previous = ts


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
    s.add_argument("--drop", help="the snapshot to delete where a VM already has its one")
    s.set_defaults(fn=cmd_deploy)
    s = sub.add_parser("timeline"); s.add_argument("--since", required=True, help="UTC, 'YYYY-MM-DD HH:MM:SS'")
    s.add_argument("--until", help="UTC, 'YYYY-MM-DD HH:MM:SS'")
    s.add_argument("--vm", action="append", help="NAME=VMID, a VM outside the inventory (repeatable)")
    s.add_argument("--all", action="store_true", help="every line, not only the milestones")
    s.add_argument("--align", action="store_true", help="shift each hive's lines by its clock offset")
    s.add_argument("--width", type=int, default=170, help="the longest line shown")
    s.add_argument("--max-lines", type=int, default=20000, help="the most lines a hive returns")
    s.set_defaults(fn=cmd_timeline)
    a = p.parse_args()
    # Progress shows as it happens even when the output goes to a pipe or a file.
    sys.stdout.reconfigure(line_buffering=True)
    load_env(a.env, a.pve_host)
    a.fn(a)


if __name__ == "__main__":
    main()
