#!/usr/bin/env python3
"""ci.py — wait for GitHub's CI on one commit, on any branch, and say how it went.

    python3 lab/ci.py COMMIT [--branch NAME] [--minutes N]

COMMIT is any name git knows for it: a short sha, a branch, HEAD. Every workflow run GitHub starts
for that commit (on NAME only, with --branch) is followed until all have ended and no new one shows
up; each prints its result and its time. On a failure the failed steps are named, and with
GITHUB_TOKEN in the environment the end of each failed job's log is printed too: the API hands job
logs only to a token, and the rest of this needs none, the repo being public. Exit status: 0 all
passed, 1 a run failed, 2 none ended in time.

A change is tested on its own branch this way (lab/logbook/METHOD.md §9). The repo defaults to
4iplatform/fluxbee; override it with FLUXBEE_GH_REPO.
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime

REPO = os.environ.get("FLUXBEE_GH_REPO", "4iplatform/fluxbee")
API = "https://api.github.com/repos/" + REPO
POLL = 20
SETTLE = 45  # runs show up a few seconds after a push: no verdict before this many seconds
LOG_TAIL = 60
PASSED = ("success", "skipped", "neutral")


class KeepRedirect(urllib.request.HTTPRedirectHandler):
    # A job's log answers with a redirect to signed storage on another host. urllib would carry
    # the Authorization header there, so the redirect is followed by hand, without it.
    def redirect_request(self, *args, **kwargs):
        return None


def get(path, raw=False):
    headers = {"Accept": "application/vnd.github+json"}
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(API + path, headers=headers)
    try:
        with urllib.request.build_opener(KeepRedirect).open(req, timeout=60) as resp:
            body = resp.read()
    except urllib.error.HTTPError as err:
        if err.code not in (301, 302, 307) or not err.headers.get("Location"):
            raise
        with urllib.request.urlopen(err.headers["Location"], timeout=60) as resp:
            body = resp.read()
    return body.decode("utf-8", "replace") if raw else json.loads(body)


def minutes(run):
    stamp = lambda s: datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ")
    return (stamp(run["updated_at"]) - stamp(run.get("run_started_at") or run["created_at"])).total_seconds() / 60


def show_failure(run):
    for job in get("/actions/runs/%d/jobs?per_page=100" % run["id"])["jobs"]:
        if job["conclusion"] in PASSED:
            continue
        steps = [s["name"] for s in job.get("steps", []) if s.get("conclusion") == "failure"]
        print("   job %s: %s; failed step: %s" % (job["name"], job["conclusion"], ", ".join(steps) or "-"))
        if not os.environ.get("GITHUB_TOKEN"):
            print("   (with GITHUB_TOKEN set, the end of its log is printed here)")
            continue
        for line in get("/actions/jobs/%d/logs" % job["id"], raw=True).splitlines()[-LOG_TAIL:]:
            # Each line starts with its timestamp, 2026-11-07T08:47:12.1234567Z.
            print("   | " + (line[29:] if line[:2] == "20" and line[28:29] == " " else line))


def main():
    p = argparse.ArgumentParser(prog="ci", description=__doc__.split("\n")[0])
    p.add_argument("commit")
    p.add_argument("--branch", help="only the runs of this branch")
    p.add_argument("--minutes", type=float, default=20, help="give up after this long (default 20)")
    a = p.parse_args()
    # Progress shows as it happens even when the output goes to a pipe or a file.
    sys.stdout.reconfigure(line_buffering=True)
    sha = subprocess.run(["git", "rev-parse", a.commit + "^{commit}"], capture_output=True,
                         text=True, check=True).stdout.strip()
    print("commit %s%s" % (sha[:12], " on " + a.branch if a.branch else ""))
    start, seen, runs = time.time(), None, []
    while time.time() - start < 60 * a.minutes:
        runs = get("/actions/runs?head_sha=%s&per_page=100" % sha)["workflow_runs"]
        if a.branch:
            runs = [r for r in runs if r["head_branch"] == a.branch]
        ids = sorted(r["id"] for r in runs)
        ended = bool(runs) and all(r["status"] == "completed" for r in runs)
        if ended and ids == seen and time.time() - start >= SETTLE:
            break
        seen = ids
        time.sleep(POLL)
    else:
        print("no verdict after %g min: %s" % (a.minutes, ", ".join(
            "%s %s" % (r["name"], r["status"]) for r in runs) or "no run started"))
        sys.exit(2)
    failed = [r for r in runs if r["conclusion"] not in PASSED]
    for r in sorted(runs, key=lambda r: r["name"]):
        print("%-26s %-9s %4.1f min  %s" % (r["name"], r["conclusion"], minutes(r), r["head_branch"]))
    for r in failed:
        print("== %s failed: %s" % (r["name"], r["html_url"]))
        show_failure(r)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
