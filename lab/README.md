# Fluxbee lab (containerized)

A self-contained, reproducible way to run a real fluxbee mesh in Docker for
testing — no VMs required. `systemd` runs as PID 1 inside each container, so the
canonical `scripts/install.sh` and the orchestrator's service lifecycle work
unchanged.

> **Status: Phase 1 — motherbee + worker.** A `worker1` container (empty box)
> joins the mesh via `add_hive` over SSH. Egress (Phase 2) is next.

## Requirements

- Docker (Desktop or engine) with **cgroup v2** (check `docker info | grep Cgroup`).
- ~**10-12 GB** memory for Docker — the first image build compiles the full Rust
  workspace + Go components (link step is memory-hungry). `CARGO_BUILD_JOBS=4`
  caps parallelism to soften this.
- `linux/amd64` engine (the vendored syncthing binary is amd64).

## Run

Two ways to get the image:

```bash
# A) Pull the prebuilt image from GHCR (no compile). Needs `docker login ghcr.io`
#    if the package is private. Built by .github/workflows/lab-image.yml.
docker compose -f lab/docker-compose.yml pull
docker compose -f lab/docker-compose.yml up -d

# B) Build it locally (~25 min full compile) — first run only.
docker compose -f lab/docker-compose.yml up -d --build
```

Then:

```bash

# Watch first boot: the one-shot install + sy-orchestrator bringing up the stack.
docker compose -f lab/docker-compose.yml logs -f motherbee

# Shell in.
docker exec -it fluxbee-motherbee bash
```

Inside the container the stack is managed by systemd:

```bash
systemctl list-units 'sy-*' 'rt-gateway*' 'fluxbee-*'
journalctl -u sy-orchestrator -f
```

## Drive admin commands

The admin HTTP API is published on `localhost:8080` (the lab rebinds
`admin.listen` to `0.0.0.0`; default is `127.0.0.1`). You can also use the
`sy-admin` CLI inside the container.

```bash
# from the host
curl -s localhost:8080/hives | jq .
# or inside
docker exec fluxbee-motherbee sy-admin --help
```

## Phase 1: add a worker over SSH

`docker compose up` also brings up `worker1` — an **empty** Linux box (sshd +
admin login `administrator`/`labpass` + Postgres, no fluxbee). Bootstrap it from
motherbee with `add_hive`, supplying the SSH credentials in the payload (this is
the caller-supplied-creds flow):

```bash
WIP=$(docker exec fluxbee-motherbee getent hosts worker1 | awk '{print $1}')
docker exec fluxbee-motherbee curl -s -X POST localhost:8080/hives \
  -H 'content-type: application/json' \
  -d "{\"hive_id\":\"worker1\",\"address\":\"$WIP\",\"ssh_user\":\"administrator\",\"ssh_password\":\"labpass\",\"role\":\"worker\"}"

curl -s localhost:8080/inventory | jq '.payload.hives'   # motherbee + worker1
```

motherbee SSHes in (key-first; falls back to the password to seed its key, then
operates over the key), copies the core binaries, installs the worker's SY stack,
and links dist-sync. `address` must be an **IP** (the orchestrator runs
`ip route get` on it).

## Reset

```bash
docker compose -f lab/docker-compose.yml down -v   # -v drops the state volume
```

First boot takes ~2 min: `lab/lab-install.sh` runs as a systemd one-shot and
provisions everything, in order:

1. lab deployment config (`wan.uplinks=[]`, `admin.listen=0.0.0.0`);
2. PostgreSQL + the three fluxbee DBs (storage/identity/cognition backend);
3. `scripts/install.sh` **unchanged** (compile is a no-op — already in the image);
4. starts `sy-orchestrator` (boots the whole SY stack);
5. `vault_put` of the Postgres pool secret (Model D' — storage then auto-restarts
   and connects);
6. waits for `hive ready`.

Then `curl localhost:8080/hives` returns the live motherbee.

## What the lab provisions vs a real host

- `wan.uplinks` is emptied (motherbee is the mesh root; don't dial the LAN).
- `admin.listen` is bound to `0.0.0.0` so the published port works.
- PostgreSQL is installed + bootstrapped, and a `resource_type=postgres` pool
  secret is written to SY.vault (the operator step every fluxbee deployment does).
- Everything else (units, embedded NATS, syncthing, the SY boot order, the
  secret flow) is the real `scripts/install.sh` + orchestrator path. **No fluxbee
  source or `install.sh` is modified** — only environment + deployment config.

## Iterating on `lab-install.sh`

It is bind-mounted, but Docker Desktop's **single-file** bind mounts go stale if
you edit the host file while the container is running (inode changes). After
editing `lab-install.sh`, recreate the container rather than restarting the unit:

```bash
docker compose -f lab/docker-compose.yml down -v && \
docker compose -f lab/docker-compose.yml up -d
```

## Notes / known rough edges

- `ufw`/nftables firewall steps run inside the container; on a privileged
  container they generally work but may warn. The egress role (later) needs
  `NET_ADMIN` + `ip_forward` and is the most container-sensitive part — a real
  VM is the high-fidelity option there.
- The image is intentionally fat (bundles build toolchains) so first boot is a
  no-op rebuild + install. A slim multi-stage variant can come later.
