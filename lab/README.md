# Fluxbee lab (containerized)

A self-contained, reproducible way to run a real fluxbee mesh in Docker for
testing — no VMs required. `systemd` runs as PID 1 inside each container, so the
canonical `scripts/install.sh` and the orchestrator's service lifecycle work
unchanged.

> **Status: Phase 0 — motherbee only.** Worker / egress containers and
> `add_hive`-over-SSH come in Phase 1-2.

## Requirements

- Docker (Desktop or engine) with **cgroup v2** (check `docker info | grep Cgroup`).
- ~**10-12 GB** memory for Docker — the first image build compiles the full Rust
  workspace + Go components (link step is memory-hungry). `CARGO_BUILD_JOBS=4`
  caps parallelism to soften this.
- `linux/amd64` engine (the vendored syncthing binary is amd64).

## Run

```bash
# Build the image and start motherbee (first build is slow — full compile).
docker compose -f lab/docker-compose.yml up -d --build

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
