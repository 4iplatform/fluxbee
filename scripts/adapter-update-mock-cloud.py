#!/usr/bin/env python3
"""Minimal mock of the Fluxbee Cloud adapter endpoints, used to exercise the
LinkedHelper adapter's *self-update mechanics* in isolation (download → verify
→ atomic swap → re-exec → finalize/rollback) on a clean host or VM.

It is NOT the real Cloud: the real update decision (`resolveAdapterUpdate`) and
artifact endpoint are unit-tested in fluxbee_cloud. This mock only needs to
return an `alive` response carrying an update directive and to serve the target
artifact bytes, so the adapter binary can run its real end-to-end update path.

Endpoints (matching the real contract shape):
  POST /api/adapters/enroll                         -> enroll result
  POST /api/adapters/<id>/alive                     -> alive + update directive
  POST /api/adapters/<id>/discovery                 -> empty discovery ack
  GET  /api/adapters/<id>/artifacts/<releaseId>     -> artifact octet-stream

The update directive is controlled by --directive:
  none            no update offered
  available       non-required upgrade offer (adapter should log, not apply)
  required        required update with the correct sha256 (adapter applies)
  required-badsha required update with a wrong sha256 (adapter must reject)
"""
import argparse
import hashlib
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ADAPTER_ID = "adp_test"
ADAPTER_SECRET = "sec_test"
TENANT_ID = "00000000-0000-0000-0000-000000000001"

CFG = {}


def _artifact_sha_size(path):
    data = open(path, "rb").read()
    return hashlib.sha256(data).hexdigest(), len(data)


def build_update_directive(base_url):
    directive = {"available": False, "required": False}
    mode = CFG["directive"]
    if mode == "none":
        return directive

    sha = CFG["sha256"]
    if mode == "required-badsha":
        # Flip the first hex nibble so integrity verification must fail.
        sha = ("f" if sha[0] != "f" else "0") + sha[1:]

    target = {
        "releaseId": CFG["release_id"],
        "version": CFG["version"],
        "url": "/api/adapters/%s/artifacts/%s" % (ADAPTER_ID, CFG["release_id"]),
        "sha256": sha,
        "size": CFG["size"],
        "sig": None,
    }
    if mode == "available":
        directive.update({"available": True, "required": False, "target": target,
                          "reason": "A newer adapter is available."})
    else:  # required or required-badsha
        directive.update({"available": True, "required": True, "target": target,
                          "reason": "Adapter is below the minimum supported version."})
    return directive


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        sys.stderr.write("[mock-cloud] " + (args[0] % args[1:]) + "\n")

    def _json(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self):
        length = int(self.headers.get("content-length", 0) or 0)
        return self.rfile.read(length) if length else b""

    def do_POST(self):
        self._read_body()
        base_url = "http://%s" % self.headers.get("host", "127.0.0.1")
        if self.path == "/api/adapters/enroll":
            return self._json(200, {"result": {
                "adapterId": ADAPTER_ID,
                "adapterSecret": ADAPTER_SECRET,
                "tenantId": TENANT_ID,
                "syncConfig": {
                    "cloudBaseUrl": base_url,
                    "discoveryUrl": "%s/api/adapters/%s/discovery" % (base_url, ADAPTER_ID),
                    "syncUrl": None,
                    "reportTo": None,
                },
            }})
        if self.path == "/api/adapters/%s/alive" % ADAPTER_ID:
            return self._json(200, {
                "ok": True,
                "serverTime": "2026-07-02T00:00:00Z",
                "adapterStatus": "accepted",
                "desiredStateChanged": False,
                "desiredStateVersion": 0,
                "desiredState": None,
                "commands": [],
                "update": build_update_directive(base_url),
                "compatibility": {"status": "unknown", "decision": "allow"},
            })
        if self.path == "/api/adapters/%s/discovery" % ADAPTER_ID:
            return self._json(200, {"result": {"received": 0, "items": []}})
        return self._json(404, {"error": "not found", "path": self.path})

    def do_GET(self):
        expected = "/api/adapters/%s/artifacts/%s" % (ADAPTER_ID, CFG["release_id"])
        if self.path == expected:
            data = open(CFG["artifact"], "rb").read()
            self.send_response(200)
            self.send_header("content-type", "application/octet-stream")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        self._json(404, {"error": "not found", "path": self.path})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8799)
    ap.add_argument("--bind", default="0.0.0.0")
    ap.add_argument("--artifact", required=True, help="path to the v2 binary to serve")
    ap.add_argument("--release-id", default="lh-adapter-0.2.0-linux-x64")
    ap.add_argument("--version", default="0.2.0")
    ap.add_argument("--directive", default="required",
                    choices=["none", "available", "required", "required-badsha"])
    a = ap.parse_args()

    if not os.path.exists(a.artifact):
        sys.exit("artifact not found: %s" % a.artifact)
    sha, size = _artifact_sha_size(a.artifact)
    CFG.update({"artifact": a.artifact, "release_id": a.release_id, "version": a.version,
                "directive": a.directive, "sha256": sha, "size": size})

    sys.stderr.write("[mock-cloud] serving %s (v=%s, sha=%s, size=%d) directive=%s on %s:%d\n"
                     % (a.release_id, a.version, sha[:12], size, a.directive, a.bind, a.port))
    ThreadingHTTPServer((a.bind, a.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
