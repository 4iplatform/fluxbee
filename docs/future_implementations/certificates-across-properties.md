# Certificates across every Fluxbee property

**Status:** direction agreed on 2026-10-29: each side issues its own certificates. Watching them all
from one place is still open. **Recorded:** 2026-10-22, the operator's request after 0.1.82
([acme-edge-certificates-v1](../acme-edge-certificates-v1.md)).

## Where things stand

- **Fluxbee installations** get their edge's certificate from an ACME CA. SY.admin issues and renews
  it with no human step (0.1.82). PROD's edge has served Let's Encrypt since 2026-10-21.
- **Every other property** still uses the brand wildcard `*.fluxbee.ai`, a paid Sectigo certificate
  valid until 2027-02-12. It is copied on several hosts, and its key can impersonate any name under
  the domain.
- **The root `fluxbee.ai`** (checked 2026-10-29) resolves to `71.182.182.108`, an nginx 1.24 that is
  none of the installation's VMs. It serves the website and passes `/api/` to an application that
  answers JSON. It serves the Sectigo wildcard, and its port 80 is closed. The DNS is Azure DNS,
  managed by hand: no CAA, and no `www`, `app`, `cloud` or `api` record.

## The direction (the operator, 2026-10-29)

Two separate sides, each issuing and renewing its own certificates:

- **Cloud:** the website and Cloud's functions (billing; the legal registration of people and
  companies; the security of the apps tied to a person; people's login). Two or three names, issued
  by Cloud's own ACME client on Cloud's own servers and IPs.
- **Fluxbee installations:** each installation's edge, with its motherbee and the names it
  generates, as today (SY.admin's ACME client, names from `hive.yaml`).

The system serving the root today is custom software that will be retired once Fluxbee runs.
Cloud's tests start on `cloud.fluxbee.ai`. Once Cloud works, it takes the root `fluxbee.ai` in a
migration. Every Sectigo certificate goes: the wildcard is not renewed, and its copies are removed.

### Why one domain is no obstacle

An ACME CA certifies names, not domains or IPs. For each name a client asks for, the CA resolves
that name and checks a challenge at the address it finds (TLS-ALPN-01 on :443, HTTP-01 on :80). So
each server proves only the names that point at it. PROD shows it today: `fluxbee.ai`
(`71.182.182.108`, Sectigo) and `hive-k3m9x7q2.fluxbee.ai` (`71.182.182.80`, the edge, Let's
Encrypt) are two servers under one domain, and neither knows about the other.

### What the two sides still share

- **The DNS zone** (Azure, by hand): both sides' records are written there.
- **Let's Encrypt's limit per registered domain:** 50 new certificates every 7 days for all of
  `fluxbee.ai`, counted across every account.
  - A renewal with the same names does not count, and an ARI renewal is exempt from every limit
    (letsencrypt.org/docs/rate-limits, updated 2026-08-05).
  - It matters only if many installations start in one week. The ways out then are a limit
    override, or a domain of their own for the installations.
- **The browser's site:** `fluxbee.ai`, `cloud.fluxbee.ai` and every `hive-<id>.fluxbee.ai` are one
  site. A cookie set with `Domain=fluxbee.ai` reaches every installation, and `SameSite` does not
  separate them.
  - Cloud's cookies are `__Host-` (host-only).
  - Cloud's state-changing requests check `Origin`.
  - On the installations' side, published pages are sandboxed (CSP `sandbox` without
    `allow-same-origin`, `connect-src 'none'`), and Archi's cookie is `__Host-`.

### The rule that keeps them apart

Neither side holds a wildcard or credentials to the DNS zone. Whoever can write `fluxbee.ai`'s DNS
can get a certificate for any name under it, the other side's names included. Each side proves its
names only by serving them. The one exception is the root's migration below: one TXT record,
written by hand, once.

### Moving the root to Cloud

1. Lower the TTL of `fluxbee.ai`'s A record a day ahead.
2. Issue Cloud's certificate for `fluxbee.ai` before the switch. The CA validates the name over
   :443 or :80 only once it points at Cloud, so the first issuance uses one DNS-01 challenge: a TXT
   record at `_acme-challenge.fluxbee.ai`, written by hand in Azure and removed after.
3. Point the A record at Cloud. From then on, renewals go through Cloud's ACME client as usual.
4. PROD's egress probe GETs `https://fluxbee.ai` (`egress.reachability_url` in PROD's `hive.yaml`;
   the code has no default). It keeps passing as long as the root serves HTTPS.

The deadline is 2027-02-12, when the wildcard expires. If the old system still serves the root
then, it needs a bridge certificate.

## Still open

- **Where Cloud's servers run.** On the PROD Proxmox, a Cloud VM takes a public IP of its own on
  `vmbr1` (`71.182.182.0/24`), as the ingress has the `.80`. Port 443 on the `.80` belongs to the
  installation.
- **Any other host still holding the wildcard.** Whatever is left on it stops working on
  2027-02-12.
- **One view of every certificate.** On 2026-10-22 the operator asked to think of it all as one
  integrated piece. Issuance is now per side, so the integrated part is watching:
  - every certificate, who issues it, and when it renews;
  - the alerts, read by the monitoring the company already runs (StatusCake).
- **CAA on `fluxbee.ai`,** restricted to the chosen CA (and its accounts, RFC 8657), once both sides
  issue.
