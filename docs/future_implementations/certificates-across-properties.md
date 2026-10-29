# Certificates across every Fluxbee property

**Status:** pending, to be designed as one piece. **Recorded:** 2026-10-22, the operator's request after
0.1.82 ([acme-edge-certificates-v1](../acme-edge-certificates-v1.md)).

## Where things stand

- **Fluxbee installations** get their edge's certificate from an ACME CA. SY.admin issues and renews
  it with no human step (0.1.82). PROD's edge has served Let's Encrypt since 2026-10-21.
- **Every other property** still uses the brand wildcard `*.fluxbee.ai`, a paid Sectigo certificate
  valid until 2027-02-12. It is copied on several hosts, and its key can impersonate any name under
  the domain. The public website `fluxbee.ai` serves it today.

- **The root `fluxbee.ai`** (checked 2026-10-29) resolves to `71.182.182.108`, an nginx 1.24 that is
  none of the installation's VMs. It serves the website and passes `/api/` to an application that
  answers JSON. It serves the Sectigo wildcard, and its port 80 is closed. The DNS is Azure DNS,
  managed by hand: no CAA, and no `www`, `app`, `cloud` or `api` record.
- **Cloud needs the root** (the operator, 2026-10-29: "cloud necesita el dominio fluxbee.ai que es
  el raíz, eso hay que revisarlo"). To review, in this order:
  - what serves the root, and whether Cloud shares it with the website or takes a name of its own;
  - where Cloud's production runs;
  - who issues the root's certificate. SY.admin's ACME client certifies only an installation's own
    names from `hive.yaml` (acme-edge-certificates-v1, O6 and D1), so it is not the issuer here.
    Whoever serves the root issues its own, with the same rule: TLS-ALPN-01 on :443, no DNS
    credentials, no wildcard.
- **What else depends on the root:**
  - PROD's egress probe checks `https://fluxbee.ai` (`egress.reachability_url`).
  - Every installation under `*.fluxbee.ai` is the same site as the root for browsers. Two
    defences hold today:
    - published pages are sandboxed (CSP `sandbox` without `allow-same-origin`,
      `connect-src 'none'`);
    - Archi's cookie is `__Host-`.

    Cloud's cookies should be `__Host-` too.

## What is wanted

- **Replace every Sectigo certificate** with automatically issued and renewed ones (Let's Encrypt or
  another ACME CA), so the wildcard is not renewed and its copies are removed.
- **Manage the certificates of all properties in one place:** website, infrastructure, Cloud, and
  the installations' edges. One view of what exists, who issues it, where it is served, when it
  renews, and alerts. Not a separate external checker per host: the lifecycle (issue, renew,
  distribute, watch) designed as one system, integrated with the monitoring the company already
  runs.

## Questions for the design

- The inventory: every name, the host or platform that serves it, how its certificate is issued
  today, and which keys are copied where.
- Per property: the host's own ACME client, the platform's managed certificate, or a central issuer
  that distributes. The trade-off is where keys and DNS credentials may live.
- Whether anything really needs a wildcard. Let's Encrypt issues wildcards only through DNS-01,
  which puts DNS credentials on the issuing host.
- How the existing monitoring reads expiry and renewal state from a single source, and what raises
  an alert.
- CAA on `fluxbee.ai`, restricted to the chosen CAs and accounts (RFC 8657), once the inventory is
  known.
- The deadline: before 2027-02-12, when the wildcard expires.
