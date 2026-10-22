# Certificates across every Fluxbee property

**Status:** pending, to be designed as one piece. **Recorded:** 2026-10-22, the operator's request after
0.1.82 ([acme-edge-certificates-v1](../acme-edge-certificates-v1.md)).

## Where things stand

- **Fluxbee installations** get their edge's certificate from an ACME CA. SY.admin issues and renews
  it with no human step (0.1.82). PROD's edge has served Let's Encrypt since 2026-10-21.
- **Every other property** still uses the brand wildcard `*.fluxbee.ai`, a paid Sectigo certificate
  valid until 2027-02-12. It is copied on several hosts, and its key can impersonate any name under
  the domain. The public website `fluxbee.ai` serves it today.

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
