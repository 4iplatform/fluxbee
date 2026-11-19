//! Where io.web dials each exposure's upstream, and how (docs/io-web-v2-stage-2-2.md §4.2).
//!
//! - **`archi`:** where Archi listens: hive.yaml's `architect.listen`, read once at boot from
//!   io.web's config dir, else `127.0.0.1:3000`, Archi's own default. A listen on `0.0.0.0` or
//!   `[::]` is dialled on loopback, any other address as it is. Archi's env override,
//!   `JSR_ARCHITECT_LISTEN`, lives in Archi's unit, which io.web cannot read.
//! - **`hive`:** a connected worker of the hive registry, at the `address` the registry records for
//!   it, read at each request. A DNS name is resolved then, and what it resolves to is checked as
//!   SY.admin checks an `address` upstream: a private range, never loopback, link-local,
//!   unspecified, multicast or an address the motherbee holds.
//! - **`address`:** the IP literal SY.admin took, checked again the same way.
//! - **Never one of Fluxbee's own ports**, whatever the kind: a misconfigured listen can never make
//!   Archi's exposure reach a Fluxbee service.
//! - **The connection:** plain TCP for HTTP/1.1, one per request, reached within 5 s, io.web's own
//!   reachability, with TCP keepalive. There is no other wait: none for the head, no cut for
//!   silence.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use fluxbee_sdk::mesh::{is_valid_hive_id, HiveRegistry, HiveRole, IO_WEB_PORT};
use fluxbee_sdk::protocol::WebExposureUpstream;
use serde::Deserialize;
use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpStream;

/// Reaching an upstream: its name resolved, its TCP connection open (§4.2).
pub const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Archi's own default listen (src/bin/sy_architect.rs, `DEFAULT_ARCHITECT_LISTEN`).
const DEFAULT_ARCHI_LISTEN: &str = "127.0.0.1:3000";

/// Archi's own port in `REFUSED_UPSTREAM_PORTS`: the one an `archi` upstream may have.
const ARCHI_PORT: u16 = 3000;

/// Fluxbee's own ports, never an upstream's, whatever its kind (§2.2), with where each is set. A
/// copy of SY.admin's `REFUSED_UPSTREAM_PORTS` (src/bin/sy_admin/exposures.rs) until the next Rust
/// SDK release moves the list into the SDK, shared by both (FINDINGS A-355): SY.admin and io.web
/// build in different cargo workspaces.
const REFUSED_UPSTREAM_PORTS: &[(u16, &str)] = &[
    (22, "SSH"),
    (80, "the edge"),
    (443, "the edge"),
    // src/bin/sy_architect.rs: Archi's own listener.
    (ARCHI_PORT, "Archi"),
    // SY.admin's HTTP API (`main`'s default listen).
    (8080, "SY.admin"),
    // src/bin/sy_orchestrator.rs: Syncthing's API.
    (8384, "Syncthing's API"),
    // src/config/mod.rs: the routers' WAN.
    (9000, "the WAN"),
    // src/bin/sy_identity.rs: identity's sync between hives.
    (9100, "identity sync"),
    (IO_WEB_PORT, "io.web"),
    (21027, "Syncthing discovery"),
    // src/bin/sy_orchestrator.rs: Syncthing's sync.
    (22000, "Syncthing"),
];

/// TCP keepalive on every socket io.web holds, the edge's and the upstreams' (§3.1, §4.2). With no
/// wait of its own left, a peer gone without a FIN or a RST (power lost, a partition) would
/// otherwise hold a stream, a WebSocket or a pending answer for ever. The kernel probes an idle
/// connection after 60 s, every 10 s, and drops it after 6 probes unanswered: about two minutes. A
/// live peer answers the probes whatever its application does, so this is reachability, never a
/// wait put on the application.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_PROBES: u32 = 6;

/// Turns TCP keepalive on for `tcp` (`KEEPALIVE_IDLE`); a socket that refuses it is still used.
pub fn keep_alive(tcp: &TcpStream) {
    let params = TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_PROBES);
    if let Err(err) = SockRef::from(tcp).set_tcp_keepalive(&params) {
        tracing::debug!(error = %err, "IO.web could not turn TCP keepalive on for a socket");
    }
}

/// Why an upstream was not reached. The client gets io.web's fixed 502 page; the log gets the kind
/// and the detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// `archi_listen`, `fluxbee_port`, `hive_unknown`, `hive_address`, `resolve`,
    /// `address_refused`, `connect`, `connect_timeout`; and, past the connection, `http_handshake`,
    /// `head` and `switched` (`proxy`).
    pub kind: &'static str,
    pub detail: String,
}

impl Failure {
    pub fn new(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// Where each kind of upstream is.
#[derive(Debug, Clone)]
pub struct Upstreams {
    /// Archi's address, from hive.yaml at boot (`archi_from_hive_yaml`), or why there is none.
    archi: Result<SocketAddr, String>,
    registry: HiveRegistry,
    /// Whether the motherbee holds an address (`host_holds_address`).
    holds: fn(&IpAddr) -> bool,
    connect_timeout: Duration,
}

impl Upstreams {
    /// Archi at `archi`, the workers in `registry`.
    pub fn new(archi: Result<SocketAddr, String>, registry: HiveRegistry) -> Self {
        Self {
            archi,
            registry,
            holds: host_holds_address,
            connect_timeout: UPSTREAM_CONNECT_TIMEOUT,
        }
    }

    /// Archi's address, or why io.web has none: the boot's log says it.
    pub fn archi(&self) -> Result<SocketAddr, &str> {
        self.archi.as_ref().copied().map_err(String::as_str)
    }

    /// A TCP connection to `upstream`, its address resolved and checked, within
    /// `connect_timeout`, with TCP keepalive; and the address it reached.
    pub async fn connect(
        &self,
        upstream: &WebExposureUpstream,
    ) -> Result<(TcpStream, SocketAddr), Failure> {
        let limit = self.connect_timeout;
        let reached = tokio::time::timeout(limit, async {
            let address = self.address(upstream).await?;
            let tcp = TcpStream::connect(address)
                .await
                .map_err(|err| Failure::new("connect", format!("{address}: {err}")))?;
            Ok((tcp, address))
        })
        .await
        .map_err(|_| {
            Failure::new(
                "connect_timeout",
                format!("the upstream was not reached within {limit:?}"),
            )
        })?;
        if let Ok((tcp, _)) = &reached {
            let _ = tcp.set_nodelay(true);
            keep_alive(tcp);
        }
        reached
    }

    /// The address io.web dials for `upstream` (§4.2's table).
    pub async fn address(&self, upstream: &WebExposureUpstream) -> Result<SocketAddr, Failure> {
        match upstream {
            WebExposureUpstream::Archi {} => self
                .archi
                .clone()
                .map_err(|reason| Failure::new("archi_listen", reason)),
            WebExposureUpstream::Hive { hive, port } => {
                check_port(*port, false).map_err(|reason| Failure::new("fluxbee_port", reason))?;
                let candidates = match self.recorded_address(hive)? {
                    Recorded::Ip(ip) => vec![ip],
                    Recorded::Name(name) => tokio::net::lookup_host((name.as_str(), *port))
                        .await
                        .map_err(|err| Failure::new("resolve", format!("{name}: {err}")))?
                        .map(|resolved| resolved.ip())
                        .collect(),
                };
                // The first address that passes; a refused one is never dialled.
                let mut refusals = Vec::new();
                for ip in candidates {
                    match check_address(&ip, self.holds) {
                        Ok(()) => return Ok(SocketAddr::new(ip, *port)),
                        Err(reason) => refusals.push(reason),
                    }
                }
                Err(if refusals.is_empty() {
                    Failure::new(
                        "resolve",
                        format!("hive '{hive}''s address resolves to nothing"),
                    )
                } else {
                    Failure::new(
                        "address_refused",
                        format!("hive '{hive}': {}", refusals.join("; ")),
                    )
                })
            }
            WebExposureUpstream::Address { address, port } => {
                check_port(*port, false).map_err(|reason| Failure::new("fluxbee_port", reason))?;
                check_address(address, self.holds)
                    .map_err(|reason| Failure::new("address_refused", reason))?;
                Ok(SocketAddr::new(*address, *port))
            }
        }
    }

    /// The address the registry records for `hive`, read now: only for a connected worker with
    /// its join done (the strict reading, as SY.admin took it at publish).
    fn recorded_address(&self, hive: &str) -> Result<Recorded, Failure> {
        if !is_valid_hive_id(hive) {
            return Err(Failure::new(
                "hive_unknown",
                format!("'{hive}' is not a hive id"),
            ));
        }
        let entries = self.registry.entries().map_err(|err| {
            Failure::new(
                "hive_unknown",
                format!(
                    "the hive registry {} cannot be read: {err}",
                    self.registry.root().display()
                ),
            )
        })?;
        let Some(entry) = entries.into_iter().find(|entry| entry.name() == hive) else {
            return Err(Failure::new(
                "hive_unknown",
                format!("hive '{hive}' is not in the hive registry"),
            ));
        };
        if entry.role() != Some(HiveRole::Worker) {
            return Err(Failure::new(
                "hive_unknown",
                format!("hive '{hive}' is not a connected worker with its join done"),
            ));
        }
        let recorded = entry.field("address").map(str::trim).unwrap_or_default();
        Recorded::parse(recorded).ok_or_else(|| {
            Failure::new(
                "hive_address",
                format!("hive '{hive}' has no address an upstream can be dialled at: {recorded:?}"),
            )
        })
    }

    #[cfg(test)]
    pub fn with_holds(mut self, holds: fn(&IpAddr) -> bool) -> Self {
        self.holds = holds;
        self
    }

    #[cfg(test)]
    pub fn with_connect_timeout(mut self, limit: Duration) -> Self {
        self.connect_timeout = limit;
        self
    }
}

/// A hive's address as the registry records it: an IP, written bare, bracketed, or with a port
/// (which is not the upstream's and is dropped), or a DNS name, resolved at the request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Recorded {
    Ip(IpAddr),
    Name(String),
}

impl Recorded {
    fn parse(recorded: &str) -> Option<Self> {
        if let Ok(ip) = recorded.parse::<IpAddr>() {
            return Some(Self::Ip(ip));
        }
        if let Ok(socket) = recorded.parse::<SocketAddr>() {
            return Some(Self::Ip(socket.ip()));
        }
        if let Some(ip) = recorded
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .and_then(|inner| inner.parse::<IpAddr>().ok())
        {
            return Some(Self::Ip(ip));
        }
        is_dns_name(recorded).then(|| Self::Name(recorded.to_ascii_lowercase()))
    }
}

/// A DNS name as the orchestrator takes a hive's address (`valid_address`): labels of letters,
/// digits and inner hyphens, 1 to 63 each, 253 characters at most.
fn is_dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// hive.yaml's sections io.web reads: Archi's `listen` alone. Any other key is Archi's or the
/// hive's, and stays unread.
#[derive(Debug, Deserialize)]
struct HiveYaml {
    architect: Option<ArchitectSection>,
}

#[derive(Debug, Deserialize)]
struct ArchitectSection {
    listen: Option<String>,
}

/// Where Archi listens, from `<config_dir>/hive.yaml` as Archi reads it (§4.2): `architect.listen`
/// when it is set and not blank, else `127.0.0.1:3000` (`archi_address`). A hive.yaml that cannot
/// be read or parsed leaves io.web without Archi's address, and Archi's exposure answers 502: Archi
/// itself does not start on such a file.
pub fn archi_from_hive_yaml(config_dir: &Path) -> Result<SocketAddr, String> {
    let path = config_dir.join("hive.yaml");
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("{} cannot be read: {err}", path.display()))?;
    let hive: HiveYaml = serde_yaml::from_str(&text)
        .map_err(|err| format!("{} cannot be parsed: {err}", path.display()))?;
    let listen = hive
        .architect
        .and_then(|section| section.listen)
        .filter(|listen| !listen.trim().is_empty());
    archi_address(listen.as_deref().unwrap_or(DEFAULT_ARCHI_LISTEN))
}

/// The address io.web dials for Archi's `listen`: an IP and a port, the unspecified address on
/// loopback, never one of Fluxbee's other ports.
fn archi_address(listen: &str) -> Result<SocketAddr, String> {
    let mut address: SocketAddr = listen.trim().parse().map_err(|_| {
        format!("architect.listen {listen:?} is not an IP address and a port, which io.web dials")
    })?;
    match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => address.set_ip(Ipv4Addr::LOCALHOST.into()),
        IpAddr::V6(ip) if ip.is_unspecified() => address.set_ip(Ipv6Addr::LOCALHOST.into()),
        _ => {}
    }
    check_port(address.port(), true).map_err(|reason| format!("architect.listen: {reason}"))?;
    Ok(address)
}

/// A port an upstream may have: not 0, and none of `REFUSED_UPSTREAM_PORTS`, but Archi's own for
/// Archi.
fn check_port(port: u16, archi: bool) -> Result<(), String> {
    if port == 0 {
        return Err("port 0 is no port".to_string());
    }
    match REFUSED_UPSTREAM_PORTS
        .iter()
        .find(|(refused, _)| *refused == port)
    {
        Some((ARCHI_PORT, _)) if archi => Ok(()),
        Some((_, what)) => Err(format!(
            "port {port} is {what}'s, one of Fluxbee's own ports: an exposure never leads to a \
             Fluxbee service"
        )),
        None => Ok(()),
    }
}

/// An address, IPv4-mapped IPv6 judged as its IPv4.
fn canonical_ip(address: &IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(*v6)),
        v4 => *v4,
    }
}

/// SY.admin's checks of an `address` upstream (src/bin/sy_admin/exposures.rs,
/// `check_private_address` and `host_holds_address`): 10/8, 172.16/12, 192.168/16 or fc00::/7, and
/// never loopback, link-local, unspecified, multicast, broadcast, or an address the motherbee
/// holds, which only Archi's kind reaches.
fn check_address(address: &IpAddr, holds: fn(&IpAddr) -> bool) -> Result<(), String> {
    let ip = canonical_ip(address);
    let refused = |what: &str| {
        Err(format!(
            "{address} is {what}: an upstream's address is a private one (10/8, 172.16/12, \
             192.168/16 or fc00::/7)"
        ))
    };
    if ip.is_loopback() {
        return refused("loopback");
    }
    if ip.is_unspecified() {
        return refused("unspecified");
    }
    if ip.is_multicast() {
        return refused("multicast");
    }
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_broadcast() {
                return refused("broadcast");
            }
            if v4.is_link_local() {
                return refused("link-local");
            }
            if !v4.is_private() {
                return refused("not private");
            }
        }
        IpAddr::V6(v6) => {
            let segment = v6.segments()[0];
            if segment & 0xffc0 == 0xfe80 {
                return refused("link-local");
            }
            if segment & 0xfe00 != 0xfc00 {
                return refused("not private");
            }
        }
    }
    if holds(&ip) {
        return Err(format!(
            "{address} is one of the motherbee's own addresses: only Archi is reached there"
        ));
    }
    Ok(())
}

/// Whether this host holds `address` on one of its interfaces, as SY.admin judges it: a UDP socket
/// binds to it only then. With `net.ipv4.ip_nonlocal_bind=1` every address binds, and every
/// address is refused: it fails closed.
fn host_holds_address(address: &IpAddr) -> bool {
    std::net::UdpSocket::bind(SocketAddr::new(*address, 0)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn holds_nothing(_: &IpAddr) -> bool {
        false
    }

    fn holds_10_0_0_9(ip: &IpAddr) -> bool {
        *ip == IpAddr::from([10, 0, 0, 9])
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "io-web-upstream-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A registry holding `entries`, each `(hive, info.yaml)`.
    fn registry(entries: &[(&str, &str)]) -> HiveRegistry {
        let root = temp_dir("registry");
        for (hive, info) in entries {
            std::fs::create_dir(root.join(hive)).unwrap();
            std::fs::write(root.join(hive).join("info.yaml"), info).unwrap();
        }
        HiveRegistry::new(root)
    }

    fn worker(hive: &str, address: &str) -> String {
        format!(
            "hive_id: {hive}\nstatus: connected\nrole: worker\naddress: {address}\njoin:\n  phase: \
             done\n"
        )
    }

    /// Archi is dialled where hive.yaml's `architect.listen` says it listens: the unspecified
    /// address on loopback, any other as it is; Archi's own default without the key; never a
    /// Fluxbee port but Archi's own; and nowhere when hive.yaml cannot be read.
    #[test]
    fn archi_is_dialled_where_hive_yaml_says_it_listens() {
        let dir = temp_dir("hive-yaml");
        let archi = |yaml: Option<&str>| {
            let path = dir.join("hive.yaml");
            match yaml {
                Some(yaml) => std::fs::write(&path, yaml).unwrap(),
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            archi_from_hive_yaml(&dir)
        };
        let at = |text: &str| Ok(text.parse::<SocketAddr>().unwrap());
        for (yaml, expected) in [
            (
                "hive_id: motherbee\narchitect:\n  listen: \"127.0.0.1:3000\"\n",
                at("127.0.0.1:3000"),
            ),
            (
                "hive_id: motherbee\narchitect:\n  listen: \"0.0.0.0:3000\"\n",
                at("127.0.0.1:3000"),
            ),
            (
                "hive_id: motherbee\narchitect:\n  listen: \"[::]:3001\"\n",
                at("[::1]:3001"),
            ),
            (
                "hive_id: motherbee\narchitect:\n  listen: \"192.168.8.207:3000\"\n",
                at("192.168.8.207:3000"),
            ),
            ("hive_id: motherbee\n", at("127.0.0.1:3000")),
            (
                "hive_id: motherbee\narchitect:\n  listen: \"  \"\n",
                at("127.0.0.1:3000"),
            ),
            ("hive_id: motherbee\narchitect: {}\n", at("127.0.0.1:3000")),
        ] {
            assert_eq!(archi(Some(yaml)), expected, "{yaml}");
        }
        for (yaml, why) in [
            (
                "architect:\n  listen: \"127.0.0.1:8080\"\n",
                "SY.admin's, one of Fluxbee's own ports",
            ),
            ("architect:\n  listen: \"0.0.0.0:9200\"\n", "io.web's"),
            ("architect:\n  listen: \"127.0.0.1:22\"\n", "SSH's"),
            (
                "architect:\n  listen: \"127.0.0.1:0\"\n",
                "port 0 is no port",
            ),
            (
                "architect:\n  listen: \"localhost:3000\"\n",
                "is not an IP address and a port",
            ),
            (
                "architect:\n  listen: \"127.0.0.1\"\n",
                "is not an IP address and a port",
            ),
            ("architect: [\n", "cannot be parsed"),
        ] {
            let err = archi(Some(yaml)).expect_err(yaml);
            assert!(err.contains(why), "{yaml}: {err}");
        }
        let err = archi(None).expect_err("no hive.yaml");
        assert!(err.contains("cannot be read"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A hive upstream is dialled at the registry's address for a connected worker, read at the
    /// request: a name resolved then and its addresses checked (here `localhost`, loopback, is
    /// refused), an IP in any of its spellings checked; a hive that is no connected worker, or
    /// has no address, is no upstream.
    #[tokio::test]
    async fn a_hive_s_name_is_resolved_and_checked() {
        let registry = registry(&[
            ("worker1", &worker("worker1", "10.0.0.5")),
            ("worker2", &worker("worker2", "localhost")),
            ("worker3", &worker("worker3", "\"[fd00::5]\"")),
            ("worker4", &worker("worker4", "10.0.0.9")),
            ("worker5", &worker("worker5", "\"10.0.0.6:22\"")),
            (
                "worker6",
                "hive_id: worker6\nstatus: connected\nrole: worker\n",
            ),
            (
                "ingress1",
                "hive_id: ingress1\nstatus: connected\nrole: ingress\naddress: 10.0.0.7\n",
            ),
            (
                "joining1",
                "hive_id: joining1\nstatus: joining\nrole: worker\naddress: 10.0.0.8\n",
            ),
            ("worker7", &worker("worker7", "\"not a name!\"")),
        ]);
        let upstreams =
            Upstreams::new(Err("unused".to_string()), registry.clone()).with_holds(holds_10_0_0_9);
        let hive = |hive: &str, port: u16| WebExposureUpstream::Hive {
            hive: hive.to_string(),
            port,
        };
        assert_eq!(
            upstreams.address(&hive("worker1", 8069)).await,
            Ok("10.0.0.5:8069".parse().unwrap())
        );
        assert_eq!(
            upstreams.address(&hive("worker3", 8069)).await,
            Ok("[fd00::5]:8069".parse().unwrap())
        );
        assert_eq!(
            upstreams.address(&hive("worker5", 8069)).await,
            Ok("10.0.0.6:8069".parse().unwrap()),
            "the recorded port is SSH's, not the upstream's"
        );
        let refused = upstreams
            .address(&hive("worker2", 8069))
            .await
            .expect_err("localhost resolves to loopback");
        assert_eq!(refused.kind, "address_refused", "{refused:?}");
        assert!(refused.detail.contains("loopback"), "{refused:?}");
        let own = upstreams
            .address(&hive("worker4", 8069))
            .await
            .expect_err("the motherbee's own address");
        assert_eq!(own.kind, "address_refused");
        assert!(own.detail.contains("motherbee's own"), "{own:?}");
        for (case, kind) in [
            ("worker6", "hive_address"),
            ("worker7", "hive_address"),
            ("ingress1", "hive_unknown"),
            ("joining1", "hive_unknown"),
            ("gone1", "hive_unknown"),
            ("../worker1", "hive_unknown"),
        ] {
            let failure = upstreams.address(&hive(case, 8069)).await.expect_err(case);
            assert_eq!(failure.kind, kind, "{case}: {failure:?}");
        }
        let port = upstreams
            .address(&hive("worker1", 9000))
            .await
            .expect_err("the WAN's port");
        assert_eq!(port.kind, "fluxbee_port");
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// An address upstream is checked again as SY.admin checked it.
    #[tokio::test]
    async fn an_address_upstream_is_checked_again() {
        let upstreams = Upstreams::new(Err("unused".to_string()), HiveRegistry::new("/nowhere"))
            .with_holds(holds_10_0_0_9);
        let address = |ip: &str, port: u16| WebExposureUpstream::Address {
            address: ip.parse().unwrap(),
            port,
        };
        for good in ["10.1.2.3", "172.16.0.1", "192.168.8.180", "fd12::1"] {
            assert!(
                upstreams.address(&address(good, 8069)).await.is_ok(),
                "{good}"
            );
        }
        for (bad, why) in [
            ("127.0.0.1", "loopback"),
            ("::1", "loopback"),
            ("0.0.0.0", "unspecified"),
            ("224.0.0.1", "multicast"),
            ("169.254.1.1", "link-local"),
            ("fe80::1", "link-local"),
            ("8.8.8.8", "not private"),
            ("2001:db8::1", "not private"),
            ("::ffff:127.0.0.1", "loopback"),
            ("10.0.0.9", "motherbee's own"),
        ] {
            let failure = upstreams.address(&address(bad, 8069)).await.expect_err(bad);
            assert_eq!(failure.kind, "address_refused", "{bad}");
            assert!(failure.detail.contains(why), "{bad}: {failure:?}");
        }
        let port = upstreams
            .address(&address("10.1.2.3", 3000))
            .await
            .expect_err("Archi's port, for another kind");
        assert_eq!(port.kind, "fluxbee_port");
        assert!(check_port(3000, true).is_ok());
        assert!(check_port(8384, true).is_err());
    }

    /// An upstream that refuses the connection, or a port nothing listens on, is a `connect`
    /// failure; one that never answers the SYN is `connect_timeout`.
    #[tokio::test]
    async fn an_unreachable_upstream_is_a_connect_failure() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let closed = probe.local_addr().unwrap();
        drop(probe);
        let upstreams = Upstreams::new(Ok(closed), HiveRegistry::new("/nowhere"));
        let failure = upstreams
            .connect(&WebExposureUpstream::Archi {})
            .await
            .expect_err("nothing listens");
        assert_eq!(failure.kind, "connect", "{failure:?}");

        let none = Upstreams::new(Err("hive.yaml: gone".to_string()), HiveRegistry::new("/x"));
        let failure = none
            .connect(&WebExposureUpstream::Archi {})
            .await
            .expect_err("no listen");
        assert_eq!(
            failure,
            Failure::new("archi_listen", "hive.yaml: gone".to_string())
        );

        // 10.255.255.1, a private address nothing on the test host answers: the SYN goes
        // unanswered, or the network refuses at once.
        let silent = Upstreams::new(Err(String::new()), HiveRegistry::new("/x"))
            .with_holds(holds_nothing)
            .with_connect_timeout(Duration::from_millis(200));
        let started = std::time::Instant::now();
        let failure = silent
            .connect(&WebExposureUpstream::Address {
                address: "10.255.255.1".parse().unwrap(),
                port: 8069,
            })
            .await
            .expect_err("unreachable");
        assert!(
            matches!(failure.kind, "connect_timeout" | "connect"),
            "{failure:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
