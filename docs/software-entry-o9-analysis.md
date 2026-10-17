# How software enters an installation — O9 analysis (for discussion)

**Status:** analysis for the operator's open decision O9 (2026-10-06,
`docs/host-posture-and-exposure-spec-v1.md` §2.2). Nothing here is decided; A-49 (the signed apt
repo, spec §3.9) waits for it.

**The operator's starting points:**

- Keep apt, the natural Linux path: *"no quiero perder el APT natural de linux"*.
- A build node inside the infrastructure that propagates the software to the motherbee with apt is
  acceptable.
- Publishing through IO.web with a token was raised.

Facts below were checked against the code at `0af5469` (0.1.60).

## 1. How software enters today

| What | Built by | Carried by | Verified by | Installed by | Trigger |
|---|---|---|---|---|---|
| **The .deb** (core binaries, base runtimes, vendored Syncthing) | fb-build, as root, from `git pull` of the branch (no signed ref) | a flat apt repo over plain HTTP (`:8900`, bound to every interface) | apt checks downloads against `Release`/`Packages`, which nothing signs (`[trusted=yes]`) | apt/dpkg on the motherbee | the operator (`ops.py deploy`, through the Proxmox guest agent) |
| **Core on spokes** | the motherbee materializes a tree per role from its own dist | Syncthing (device-ID authenticated, static internal addresses) | the spoke checks its local manifest's hash against the hash in `SYSTEM_UPDATE`, which the motherbee reads from its own dist; then each binary's hash | the spoke's orchestrator, as root | `update category=core` from the motherbee's admin |
| **Vendored Syncthing** | inside the .deb | dist/vendor through Syncthing | the vendor manifest in the same folder | the orchestrator, as root, when the hash differs | boot, watchdog, `update category=vendor` |
| **Runtimes** (IO, AI, WF nodes) | published on the motherbee: SY.admin, Archi's upload/publish, AI-generated packages | dist/runtimes through Syncthing, to workers | presence and the exec bit only; no per-file hash, no signature | run as root by the orchestrator (`systemd-run`, no `User=`) | `run_node`, rebinds, boot |
| **OPA policy** | compiled on the motherbee | dist/policy, to every hive | a manifest sha256 in the same folder | SY.opa.rules, every 5 s | a new compiled policy |
| **Configuration** | rendered by the motherbee | written over SSH once, at the join | — | the join | `add_hive`; units are re-rendered by the running orchestrator |

## 2. Where trust breaks today

1. **The repo is unsigned and served over HTTP** (A-49). Whoever answers at the repo address installs
   root software on the motherbee. The build itself is not pinned to a signed source.
2. **After dpkg, nothing anchors back to the build.** Manifests travel next to the bytes they
   describe (core, vendor, OPA): their hashes prove a complete transfer, not who wrote it. The only
   outside anchor, the core hash in `SYSTEM_UPDATE`, is read from the motherbee's own dist.
3. **Spokes install what Syncthing delivers.** Whoever holds the motherbee's Syncthing identity
   reaches root on every spoke (A-51's open part). On workers and the egress Syncthing even runs as
   root (A-58).
4. **Runtime packages are neither signed nor hashed, and run as root.** They come in through
   SY.admin, Archi's upload (no auth, loopback only), AI-generated packages and the `blob/active`
   round trip.

Who may trigger a publish or an update on the motherbee is the separate, postponed A-22.

## 3. The options

### A. Fluxbee builds and signs; each installation keeps apt (recommended)

- **Build:** one build infrastructure owned by Fluxbee (today fb-build), pinned to a signed source
  ref, signs the repo (A-49: `InRelease` with `Origin: Fluxbee`) with a release key that never
  leaves it.
- **Entry into an installation:** apt, as today. The motherbee points at Fluxbee's repo directly
  when it has outbound access, or at a small apt mirror inside the infrastructure that serves the
  same signed files. The mirror holds no key and builds nothing, so a compromised mirror cannot forge
  a release. The operator's "node inside the infra" becomes a repo node: it propagates, it does not
  build.
- **On the motherbee:** apt verifies Fluxbee's signature (`signed-by`, A-49).
- **Down to the spokes:** the .deb carries a core and vendor manifest signed with the same release
  key. The motherbee forwards it through Syncthing as today; each spoke's orchestrator verifies the
  signature (the release public key pinned at the join) before installing, instead of trusting a
  hash read from the motherbee's dist. That closes A-51's open part for core and vendor.
- **Runtimes and OPA,** which are made inside the installation, not by Fluxbee: a manifest with
  per-file hashes, signed by a key that only the motherbee's root holds (never the Syncthing user),
  pinned by spokes at the join.
- **Cost:** A-49 (designed); a signed core/vendor manifest and its check on spokes (new); a
  motherbee root key for runtimes and OPA (new); a rotation runbook for both keys.

### B. A build node inside each installation

- **What it is:** the operator's suggestion, taken literally: each site builds its own .deb and
  serves it with apt.
- **It works with apt as is,** but the signature then attests that build node, not Fluxbee's source,
  and its key becomes a high-value target on the customer's network.
- **Two builds of one commit are not comparable:** `build_id` is the build time, baked into every
  component and into the vendor manifest, so their manifest hashes differ. Reproducibility is
  untested.
- **Each site needs a toolchain box:** today's is 8 GB / 4 vCPU / 80 GB, 6–55 min per build, with
  GitHub and registry access.

### C. Fluxbee Cloud pushes through IO.web with a token

- **IO.web does not exist yet,** and its spec excludes running pushed code.
- **A token proves passage, not authorship:** it shows the request passed the edge, in the DMZ, not
  who built the bytes. The motherbee would still need Fluxbee's signature (the O5(a) pattern).
- **Core is dpkg-owned on the motherbee,** so a pushed release still lands in a local apt source.
- **So C is a delivery channel on top of A** (Cloud fills the local mirror), not a trust model of its
  own. It belongs to the Cloud phase.

## 4. Recommendation

A, in steps, each validated on 8.x like the posture stages:

1. A-49: the signed repo, with the build pinned to a signed source ref.
2. A signed core and vendor manifest, verified by spokes before they install.
3. The motherbee's root key for runtimes and OPA, verified by spokes.
4. The apt mirror inside the infrastructure, when the first customer site needs one.
5. Cloud filling that mirror (C), in the Cloud phase.

## 5. Questions for the operator

1. **Who builds and signs:** Fluxbee centrally (A) or each site (B)?
2. **How the motherbee reaches the repo:** directly, or always through a mirror inside the
   infrastructure?
3. **When spokes verify:** signed manifests checked on spokes as part of the core work now, or after
   the Cloud phase?
4. **Runtimes and OPA** signed by a motherbee root key: OK?

## 6. Checked along the way

- **Only the motherbee reaches a spoke's protected actions.** The router denies protected SYSTEM
  authority to an origin learned through the hub (test
  `via_hub_src_is_denied_system_authority_but_direct_src_is_allowed`). Spokes still reach the
  motherbee's protected actions (A-22, postponed; A-48 for `SYSTEM_CORE_ROLLBACK`).
- **Doc drift to fix:**
  - HANDBOOK says there is no core rollback command, but `core_rollback` exists.
  - It names `dpkg-scanpackages`; the publish script uses `apt-ftparchive`.
  - `packaging-and-build.md` names VM 210 for the build host; PROD uses VM 110.
  - `07-operaciones.md` lists an older Depends set and a single dist folder.
  - Two comments cite U-6 for "hive.yaml is never re-emitted"; U-6 is the vendor race.
  - An orchestrator comment says the motherbee "runs from dist/core"; its units run `/usr/bin`.
