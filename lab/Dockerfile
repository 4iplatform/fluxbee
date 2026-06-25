# Fluxbee lab — single systemd-enabled image (motherbee / worker / egress).
#
# Phase 0 target: boot motherbee. The image bundles build + runtime deps so the
# canonical scripts/install.sh runs unchanged at first boot (under systemd as
# PID 1). Heavy Rust compile is cached at image-build time; install.sh's rebuild
# at boot is then a fast freshness no-op before it installs units + binaries.
#
# Build context is the repo root (see docker-compose.yml).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    PATH=/root/.cargo/bin:/usr/local/go/bin:/usr/lib/go/bin:$PATH \
    # Cap parallel codegen/link to avoid OOM on modest Docker Desktop memory.
    CARGO_BUILD_JOBS=4

# --- System: systemd init + runtime deps + build deps ------------------------
RUN apt-get update && apt-get install -y --no-install-recommends \
      systemd systemd-sysv dbus dbus-user-session \
      openssh-server openssh-client sudo \
      ufw nftables iproute2 iptables iputils-ping procps \
      ca-certificates curl git file python3 xz-utils \
      build-essential pkg-config libssl-dev cmake clang \
      protobuf-compiler libprotobuf-dev golang \
  && rm -rf /var/lib/apt/lists/*

# Rust toolchain (rustup, pinned stable, minimal profile).
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain stable --profile minimal

# systemd hygiene for containers: drop units that fail / are pointless in a
# container so the boot log stays readable.
RUN systemctl mask \
      systemd-udevd.service systemd-udevd-kernel.socket systemd-udevd-control.socket \
      systemd-modules-load.service sys-kernel-config.mount sys-kernel-debug.mount \
      getty.target console-getty.service \
      systemd-firstboot.service systemd-resolved.service 2>/dev/null || true

# --- Source + pre-build (cached compile) -------------------------------------
WORKDIR /opt/fluxbee
COPY . /opt/fluxbee

# Mirror scripts/install.sh's build section so boot-time install is a no-op.
# Cache the cargo registry/git downloads across rebuilds (compiled artifacts stay
# in the image layer at target/, so boot-time install.sh finds them).
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo build --release --bins \
  && cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov \
  && cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner \
  && cargo build --release --manifest-path nodes/io/Cargo.toml -p io-api -p io-slack \
  && (cd go/sy-opa-rules && go build -o sy-opa-rules .) \
  && (cd go/sy-timer && go build -o sy-timer .) \
  && (cd go/sy-wf-rules && go build -o sy-wf-rules .) \
  && (cd go/nodes/wf/wf-generic && go build -o wf-generic .)

# --- Lab boot wiring ---------------------------------------------------------
COPY lab/lab-install.sh /usr/local/bin/lab-install.sh
COPY lab/fluxbee-lab-install.service /etc/systemd/system/fluxbee-lab-install.service
RUN chmod 0755 /usr/local/bin/lab-install.sh \
  && mkdir -p /run/sshd \
  && systemctl enable fluxbee-lab-install.service \
  && systemctl enable ssh

# systemd as PID 1.
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
