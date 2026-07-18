#!/usr/bin/env python3
"""fc_emulator — emulate *Fluxbee Cloud* (FC) hitting the public ingress from OUTSIDE.

FC is the external product (a website) that drives internal provisioning through the edge.
It is NOT a mesh node: it only ever speaks HTTP(S) to the edge's public URL, exactly like a
real browser/backend would. This tool models that faithfully so the *whole* circuit can be
exercised end-to-end with real security:

    FC (this tool)  ── HTTPS + Authorization: Bearer <token> ──►  SY.edge (ingress :443/:8443)
                                                                     │  validates TLS + token,
                                                                     │  strips the bearer (§3),
                                                                     ▼  forwards by Option Z
                                                              IO.cloud@motherbee
                                                                     │  translate {op,tenant,params}
                                                                     ▼
                                                              SY.admin / SY.vault

The edge exposes one channel as `<edge>/e/<ich>`; the request body is the cloud op envelope
`{op, tenant_id, params}` (spec docs/io-cloud-spec-v1.md §1.2 — the tenant is Cloud-asserted).
The entry `token` is the `IO_CLOUD_SECRET` service bearer configured in both IO.cloud and FC; SY.admin
stores it in vault during externalize and the edge checks `Authorization: Bearer <token>` at the door.

Usage:
  # create an active tenant (no tenant_id is sent for this operation)
  fc_emulator.py --edge https://192.168.4.41:8443 --ich ich:<uuid> --token <tok> --insecure \
      create-tenant --name Acme --domain acme.example

  # store a provider token (the "guardar tokens" path)
  fc_emulator.py --edge https://192.168.4.41:8443 --ich ich:<uuid> --token <tok> --insecure \
      put-token --key wapp_token:acme --value-token sk-live-xyz --resource-type bearer_token

  # launch a node (the "lanzar nodos" path; needs a published io.wapp runtime)
  fc_emulator.py --edge https://... --ich ich:<uuid> --token <tok> --cafile edge-ca.pem \
      provision-node --node-name IO.acme --runtime io.wapp

  # full security probe — proves the lock, not just the path:
  #   no token -> 401, wrong token -> 401, correct token -> 200 (put_token stored)
  fc_emulator.py --edge https://192.168.4.41:8443 --ich ich:<uuid> --token <tok> --insecure probe

TLS:
  --cafile <pem>   trust this CA/cert (real *.fluxbee.ai or a lab self-signed CA)
  --insecure       skip TLS verification (LAB ONLY — never for a real FC deployment)
  (plain http:// edge URLs work too, for a plaintext dev ingress.)

Exit code: 0 = success / all probe assertions passed; non-zero otherwise. stdlib only.
"""
import argparse
import json
import ssl
import sys
import urllib.error
import urllib.request
import uuid

DEFAULT_TENANT = "tnt:00000000-0000-0000-0000-000000000001"


class FluxbeeCloudClient:
    """A minimal, faithful external client for one externalized edge channel."""

    def __init__(self, edge_base, ich, token=None, tenant=DEFAULT_TENANT,
                 cafile=None, insecure=False, timeout=15):
        self.url = "%s/e/%s" % (edge_base.rstrip("/"), ich)
        self.ich = ich
        self.token = token
        self.tenant = tenant
        self.timeout = timeout
        if edge_base.lower().startswith("https"):
            ctx = ssl.create_default_context(cafile=cafile)
            if insecure:
                ctx.check_hostname = False
                ctx.verify_mode = ssl.CERT_NONE
            self._ctx = ctx
        else:
            self._ctx = None  # plaintext http

    def call(self, op, params, token="__default__", tenant="__default__"):
        """POST one cloud op. `token` overrides the instance token (for probe cases:
        None = send no Authorization header; a string = send that bearer). Returns
        (http_status, parsed_body_or_text)."""
        tok = self.token if token == "__default__" else token
        tenant_id = self.tenant if tenant == "__default__" else tenant
        envelope = {
            "op": op,
            "request_id": str(uuid.uuid4()),
            "params": params,
        }
        if tenant_id is not None:
            envelope["tenant_id"] = tenant_id
        body = json.dumps(envelope).encode()
        headers = {"Content-Type": "application/json"}
        if tok is not None:
            headers["Authorization"] = "Bearer %s" % tok
        req = urllib.request.Request(self.url, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req, context=self._ctx, timeout=self.timeout) as r:
                return r.status, _parse(r.read())
        except urllib.error.HTTPError as e:
            # The edge returns structured JSON errors (401 UNAUTHORIZED, 502, 503, ...).
            return e.code, _parse(e.read())
        except urllib.error.URLError as e:
            sys.exit("connection error to %s: %s\n(is the ingress up + reachable? forwarder / "
                     "LAN?)" % (self.url, e.reason))

    # --- cloud ops (spec io-cloud-spec-v1.md) ----------------------------------------
    def create_tenant(self, name, domain=None, status="active"):
        params = {"name": name, "status": status}
        if domain:
            params["domain"] = domain
        return self.call("create_tenant", params, tenant=None)

    def put_token(self, key, value_token, resource_type, owner_node=None):
        params = {
            "key": key,
            "value": {"token": value_token},
            "resource_type": resource_type,
        }
        if owner_node:
            params["owner_node"] = owner_node
        return self.call("put_token", params)

    def provision_node(self, node_name, runtime=None, runtime_version="current", config=None):
        params = {"node_name": node_name}
        if runtime:
            params["runtime"] = runtime
        if runtime_version:
            params["runtime_version"] = runtime_version
        if config is not None:
            params["config"] = config
        return self.call("provision_node", params)


def _parse(raw):
    try:
        return json.loads(raw.decode())
    except Exception:
        return raw.decode("utf-8", "replace")


def _ok(body):
    return isinstance(body, dict) and body.get("status") == "ok"


def _emit(status, body):
    print("HTTP %s" % status)
    print(json.dumps(body, indent=2) if isinstance(body, dict) else body)


def cmd_create_tenant(c, a):
    status, body = c.create_tenant(a.name, a.domain, a.status)
    _emit(status, body)
    return 0 if (status == 200 and _ok(body)) else 1


def cmd_put_token(c, a):
    status, body = c.put_token(a.key, a.value_token, a.resource_type, a.owner_node)
    _emit(status, body)
    return 0 if (status == 200 and _ok(body)) else 1


def cmd_provision_node(c, a):
    config = None
    if a.config_json:
        try:
            config = json.loads(a.config_json)
        except json.JSONDecodeError as exc:
            sys.exit("invalid --config-json: %s" % exc)
        if not isinstance(config, dict):
            sys.exit("--config-json must decode to a JSON object")
    status, body = c.provision_node(a.node_name, a.runtime, a.runtime_version, config)
    _emit(status, body)
    # A missing io.wapp runtime is a real, expected error until one is published — surface it
    # honestly rather than pretending success.
    return 0 if (status == 200 and _ok(body)) else 1


def cmd_probe(c, a):
    """Prove the LOCK end-to-end: no token -> 401, wrong token -> 401, correct token -> 200."""
    failures = []

    def check(label, expect_status, got_status, extra_ok=True):
        ok = got_status == expect_status and extra_ok
        print("  [%s] %s: HTTP %s (want %s)" % ("PASS" if ok else "FAIL", label, got_status, expect_status))
        if not ok:
            failures.append(label)

    probe_params = {"key": "wapp_token:probe", "value": {"token": "probe-secret"},
                    "resource_type": "bearer_token"}

    print("Probing %s" % c.url)
    s, _ = c.call("put_token", probe_params, token=None)
    check("no-token rejected", 401, s)
    s, _ = c.call("put_token", probe_params, token="wrong-" + (c.token or "x"))
    check("wrong-token rejected", 401, s)
    s, b = c.call("put_token", probe_params, token="__default__")
    check("correct-token accepted + stored", 200, s, extra_ok=_ok(b) and isinstance(b, dict)
          and isinstance(b.get("result"), dict) and b["result"].get("status") == "ok")

    if failures:
        print("PROBE FAILED: %s" % ", ".join(failures))
        return 1
    print("PROBE PASSED — the secure circuit is closed and the door enforces the token.")
    return 0


def main():
    p = argparse.ArgumentParser(prog="fc_emulator", description="Emulate Fluxbee Cloud against the edge")
    p.add_argument("--edge", required=True, help="edge public base URL, e.g. https://192.168.4.41:8443")
    p.add_argument("--ich", required=True, help="the externalized channel ICH (ich:<uuid>)")
    p.add_argument("--token", default=None, help="entry bearer token (from externalize; §8)")
    p.add_argument("--tenant", default=DEFAULT_TENANT, help="tenant_id the Cloud asserts (§1.2)")
    p.add_argument("--cafile", default=None, help="CA/cert PEM to trust for HTTPS")
    p.add_argument("--insecure", action="store_true", help="skip TLS verification (LAB ONLY)")
    p.add_argument("--timeout", type=int, default=15)
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("create-tenant", help="create an active tenant via the edge")
    s.add_argument("--name", required=True)
    s.add_argument("--domain", default=None)
    s.add_argument("--status", choices=("active", "pending", "suspended"), default="active")
    s.set_defaults(fn=cmd_create_tenant)

    s = sub.add_parser("put-token", help="store a provider token via the edge")
    s.add_argument("--key", required=True)
    s.add_argument("--value-token", required=True)
    s.add_argument("--resource-type", default="bearer_token")
    s.add_argument("--owner-node", default=None,
                   help="optional IO.* owner; omit for the tenant pool")
    s.set_defaults(fn=cmd_put_token)

    s = sub.add_parser("provision-node", help="launch a node via the edge (needs an io.wapp runtime)")
    s.add_argument("--node-name", required=True)
    s.add_argument("--runtime", default=None)
    s.add_argument("--runtime-version", default="current")
    s.add_argument("--config-json", default=None, help="runtime config as one JSON object")
    s.set_defaults(fn=cmd_provision_node)

    sub.add_parser("probe", help="full security probe: no/wrong token 401, correct 200").set_defaults(fn=cmd_probe)

    a = p.parse_args()
    client = FluxbeeCloudClient(a.edge, a.ich, token=a.token, tenant=a.tenant,
                                cafile=a.cafile, insecure=a.insecure, timeout=a.timeout)
    sys.exit(a.fn(client, a))


if __name__ == "__main__":
    main()
