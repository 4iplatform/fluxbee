use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::policy::{ActionClass, ActionResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub routing: Routing,
    pub meta: Meta,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    pub src: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_l2_name: Option<String>,
    #[serde(deserialize_with = "deserialize_dst")]
    pub dst: Destination,
    pub ttl: u8,
    pub trace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Destination {
    Unicast(String),
    Broadcast,
    Resolve,
}

impl Serialize for Destination {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Destination::Unicast(value) => serializer.serialize_str(value),
            Destination::Broadcast => serializer.serialize_str("broadcast"),
            Destination::Resolve => serializer.serialize_none(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Meta {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_ilk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_ilk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ich: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Legacy/historical field from the pre-thread cognition model.
    pub ctx: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Legacy/historical field from the pre-thread cognition model.
    pub ctx_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Legacy/historical field from the pre-thread cognition model.
    pub ctx_window: Option<Vec<CtxTurn>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_package: Option<MemoryPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_class: Option<ActionClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_result: Option<ActionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_detail_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    /// True when this message is a tap copy emitted by the router as part
    /// of a `TapEntry` fanout. Set ONLY by the router on the secondary copy
    /// it generates; producers and consumers must never set it. Its purpose
    /// is loop prevention: a tap copy must not itself trigger further tap
    /// fanout (single-hop tap semantics). Serialized only when true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub via_tap: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPackage {
    pub package_version: u32,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_context: Option<MemoryContextSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_reason: Option<MemoryReasonSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<MemoryContextSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<MemoryReasonSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<MemorySummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub episodes: Vec<EpisodeSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<MemoryPackageTruncated>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryContextSummary {
    pub context_id: String,
    pub label: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReasonSummary {
    pub reason_id: String,
    pub label: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySummary {
    pub memory_id: String,
    pub summary: String,
    pub weight: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_reason_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeSummary {
    pub episode_id: String,
    pub title: String,
    pub intensity: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPackageTruncated {
    pub applied: bool,
    pub dropped_contexts: u32,
    pub dropped_reasons: u32,
    pub dropped_memories: u32,
    pub dropped_episodes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtxTurn {
    pub seq: u64,
    pub ts: String,
    pub from: String,
    #[serde(rename = "type")]
    pub turn_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHelloPayload {
    pub uuid: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterHelloPayload {
    pub router_id: String,
    pub router_name: String,
    pub shm_name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanHelloPayload {
    pub protocol: String,
    pub router_id: String,
    pub router_name: String,
    pub hive_id: String,
    pub capabilities: Vec<String>,
    pub timers: WanTimers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanTimers {
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanAcceptPayload {
    pub peer_router_id: String,
    pub negotiated: WanNegotiated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanNegotiated {
    pub protocol: String,
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanRejectPayload {
    pub reason: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAnnouncePayload {
    pub uuid: String,
    pub name: String,
    pub status: String,
    pub vpn_id: u32,
    pub router_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreachablePayload {
    pub original_dst: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtlExceededPayload {
    pub original_dst: String,
    pub last_hop: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EchoPayload {}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EchoReplyPayload {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSyncPayload {
    pub timestamp_utc: String,
    pub epoch_ms: u64,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawPayload {
    pub uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangedPayload {
    pub subsystem: String,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub auto_apply: Option<bool>,
    pub version: u64,
    #[serde(default)]
    pub config: Value,
}

/// Broadcast payload emitted by SY.vault whenever a secret changes
/// (put / rotate / delete / rollback). Carries metadata ONLY — never the
/// plaintext value. Consumers that match the resource interest filter call
/// `vault_get` themselves with their own caller credentials.
///
/// Mirrors the `CONFIG_CHANGED` broadcast pattern (router socket,
/// `Destination::Broadcast`, scope=global) so consumers receive it through
/// their dispatcher route profile — no separate pub-sub channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSecretChangedPayload {
    /// Operation that triggered the event.
    pub op: VaultSecretOp,
    /// Canonical resource_type (e.g. "openai", "postgres", "slack"). Always
    /// present and normalized.
    pub resource_type: String,
    /// Owning tenant. `DEFAULT_ROOT_TENANT_ID` for infrastructure-wide
    /// system pool secrets; `"tnt:<uuid>"` for tenant-scoped secrets.
    pub tenant_id: String,
    /// Owner ILK when the secret is dedicated to a single caller. `None`
    /// (or empty string) when the secret lives in the pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk: Option<String>,
    /// New version number after the operation. Increments per change.
    /// `0` for delete events (the version no longer exists).
    pub version: i64,
    /// Vault key (`sys:openai-api-key`, etc.). Useful for logs but
    /// consumers should match by `(resource_type, tenant_id, ilk)` —
    /// the key is an opaque identifier and may differ across hives.
    pub key: String,
    /// Hive the event belongs to. Useful for multi-hive deployments to
    /// filter cross-hive noise.
    pub hive_id: String,
    /// Epoch milliseconds when SY.vault committed the change.
    pub at_ms: i64,
}

/// Vault secret mutation type carried in `VaultSecretChangedPayload.op`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultSecretOp {
    Put,
    Rotate,
    Delete,
    Rollback,
}

impl VaultSecretOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Rotate => "rotate",
            Self::Delete => "delete",
            Self::Rollback => "rollback",
        }
    }
}

/// Filter a consumer applies when listening for `VAULT_SECRET_CHANGED`
/// broadcasts. The semantics match `resolve_resource`: a consumer cares
/// about events that match its `resource_type` AND that fall under its
/// pool/dedicated match rules.
#[derive(Debug, Clone)]
pub struct VaultSecretInterest<'a> {
    /// Required: only events for this resource_type match.
    pub resource_type: &'a str,
    /// The consumer's tenant_id (e.g. `DEFAULT_ROOT_TENANT_ID` for SY).
    pub my_tenant: &'a str,
    /// The consumer's self ILK. Used to match dedicated secrets.
    pub my_ilk: Option<&'a str>,
    /// When `true`, also match events for the fixed Fluxbee root tenant
    /// regardless of `my_tenant` — mirrors the root-pool universal read
    /// rule for system callers.
    pub system_caller: bool,
}

impl VaultSecretChangedPayload {
    /// True when this event matches what the consumer cares about. Mirrors
    /// the `authorize_read` rules of SY.vault so the consumer only reacts
    /// to secrets it could actually read.
    pub fn matches_interest(&self, interest: &VaultSecretInterest<'_>) -> bool {
        if self.resource_type != interest.resource_type {
            return false;
        }
        let secret_ilk = self.ilk.as_deref().map(str::trim).filter(|v| !v.is_empty());
        let my_ilk = interest.my_ilk.map(str::trim).filter(|v| !v.is_empty());
        // (1) Dedicated match.
        if let (Some(owner), Some(mine)) = (secret_ilk, my_ilk) {
            return owner == mine;
        }
        // Pool match — secret has no ilk.
        if secret_ilk.is_none() {
            // (2a) Tenant pool.
            if self.tenant_id == interest.my_tenant {
                return true;
            }
            // (2b) Root-tenant pool universal for system callers — secrets
            // in the hive's root tenant (`DEFAULT_ROOT_TENANT_ID`, alias
            // `fluxbee`) are infrastructure-wide; any SY system caller
            // reads them regardless of which tenant they themselves
            // belong to.
            if interest.system_caller && self.tenant_id == crate::identity::DEFAULT_ROOT_TENANT_ID {
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaPayload {
    pub hive: String,
    #[serde(default)]
    pub router_id: String,
    #[serde(default)]
    pub router_name: String,
    pub seq: u64,
    pub timestamp: String,
    pub nodes: Vec<LsaNode>,
    pub routes: Vec<LsaRoute>,
    pub vpns: Vec<LsaVpn>,
    pub taps: Vec<LsaTap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaNode {
    pub uuid: String,
    pub name: String,
    pub vpn_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaRoute {
    pub prefix: String,
    pub match_kind: String,
    pub action: String,
    pub next_hop_hive: String,
    pub metric: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaVpn {
    pub pattern: String,
    pub match_kind: String,
    pub vpn_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaTap {
    pub match_src: String,
    pub match_dst: String,
    pub target: String,
    pub mode: String,
    pub enabled: bool,
}

/// Hub-authored multi-hop reachability advertisement payload (`MSG_WAN_REACHABILITY`).
/// `origin_hive`/`router_id` identify the AUTHORING hub (must equal the mTLS-authenticated peer
/// bucket on ingest, same as `LsaPayload.hive`). `entries` list spoke nodes reachable via the hub;
/// each entry's `hive_id` is the node's ORIGIN hive (the spoke it lives on), never the hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanReachabilityPayload {
    pub origin_hive: String,
    #[serde(default)]
    pub router_id: String,
    #[serde(default)]
    pub router_name: String,
    pub seq: u64,
    pub timestamp: String,
    pub entries: Vec<WanReachabilityEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanReachabilityEntry {
    pub uuid: String,
    pub name: String,
    /// The node's origin hive (the spoke it is locally attached to), NOT the vouching hub.
    pub hive_id: String,
    pub vpn_id: u32,
}

impl Message {
    /// Returns the L2 canonical name of the sender as stamped by the router.
    ///
    /// This field is set authoritatively by the local router on every delivery;
    /// node code should read it here rather than performing any SHM lookup.
    /// Returns `None` only when the message was not yet delivered by a router
    /// (e.g. messages constructed locally before sending).
    pub fn source_l2_name(&self) -> Option<&str> {
        self.routing.src_l2_name.as_deref()
    }
}

pub const SYSTEM_KIND: &str = "system";

/// Case-insensitive check that a message-kind string is the SYSTEM kind.
///
/// The router classifies control-plane traffic case-insensitively
/// (`eq_ignore_ascii_case`, router `is_system_kind`), so nodes MUST agree:
/// a case-sensitive `== SYSTEM_KIND` on the node side would silently drop an
/// authorized "System"/"SYSTEM" frame that the router already authorized and
/// forwarded (edge/ingress audit item #14 — a consistency/availability
/// divergence, not an authz bypass; the router remains the authority gate).
/// Every node-side system-kind classification MUST go through this helper.
#[inline]
pub fn is_system_kind(msg_type: &str) -> bool {
    msg_type.eq_ignore_ascii_case(SYSTEM_KIND)
}

pub const MSG_HELLO: &str = "HELLO";
pub const MSG_ANNOUNCE: &str = "ANNOUNCE";
pub const MSG_UNREACHABLE: &str = "UNREACHABLE";
pub const MSG_TTL_EXCEEDED: &str = "TTL_EXCEEDED";
pub const MSG_ECHO: &str = "ECHO";
pub const MSG_ECHO_REPLY: &str = "ECHO_REPLY";
pub const MSG_LSA: &str = "LSA";
/// Hub-authored multi-hop reachability advertisement (Option B, edge-multihop-reachability-spec-v1).
/// Emitted ONLY by a gateway (hub) in its own authenticated bucket, listing spoke nodes reachable
/// through it so a spoke can resolve+forward to a non-adjacent hive. It is DISTINCT from `LSA`: it
/// never writes the identity-bearing LSA snapshot and its entries are treated as `via_hub`
/// (transitively learned) — admitted for data delivery but denied SYSTEM authority.
pub const MSG_WAN_REACHABILITY: &str = "WAN_REACHABILITY";
pub const MSG_WAN_ACCEPT: &str = "WAN_ACCEPT";
pub const MSG_WAN_REJECT: &str = "WAN_REJECT";
pub const MSG_TIME_SYNC: &str = "TIME_SYNC";
pub const MSG_WITHDRAW: &str = "WITHDRAW";
pub const MSG_CONFIG_CHANGED: &str = "CONFIG_CHANGED";
/// Protocol version advertised in the WAN `HELLO` between hives.
///
/// TELEMETRY, NOT A GATE. In fluxbee the HASH is the gate and the VERSION is a report — the same
/// split the OPA reload uses (different version → warn, different hash → reject).
///
/// ⛔ Do NOT start rejecting peers whose version differs. That is self-fencing: the cross-hive
/// `SYSTEM_UPDATE` travels over the mesh and needs LSA visibility of its target, so cutting off
/// the outdated peer severs the only channel capable of updating it (U-4a).
pub const MESH_PROTOCOL_VERSION: &str = "fluxbee/1.16";

pub const MSG_VAULT_SECRET_CHANGED: &str = "VAULT_SECRET_CHANGED";

/// Stamped in `Meta.action` (NOT in the payload) on the `VAULT_SECRET_CHANGED` broadcasts
/// `SY.vault` emits for every secret it already holds during ITS OWN startup.
///
/// These are a re-announcement, not a change — they exist to rescue consumers that raced ahead
/// of the vault at boot. Any consumer whose reaction is DESTRUCTIVE must check this: `SY.edge`
/// reacts to a TLS change by exiting for a systemd restart, so before this was honoured every
/// `sy-vault` bounce recycled a perfectly healthy public HTTPS listener (PB-9).
pub const VAULT_BOOTSTRAP_ACTION: &str = "bootstrap";
pub const MSG_CONFIG_GET: &str = "CONFIG_GET";
pub const MSG_CONFIG_SET: &str = "CONFIG_SET";
pub const MSG_CONFIG_RESPONSE: &str = "CONFIG_RESPONSE";
pub const MSG_OPA_RELOAD: &str = "OPA_RELOAD";
pub const MSG_NODE_STATUS_GET: &str = "NODE_STATUS_GET";
pub const MSG_NODE_STATUS_GET_RESPONSE: &str = "NODE_STATUS_GET_RESPONSE";

// SY.edge URL-service command plane (SY.admin -> SY.edge). These are VERIFIED SERVICE
// DIRECTIVES, not config: SY.admin opens/closes one public URL (an `ICH`) on the edge's
// forwarding table on behalf of the owning IO node. They are addressed Unicast to the
// edge and acked (request/response), so they route cross-hive like any node RPC — and
// they are DELIBERATELY distinct from CONFIG_CHANGED / `node_config` (§9), which is the
// edge's OWN configuration (listen, TLS, log_level, DNS resolver).
pub const MSG_EDGE_OPEN_URL: &str = "EDGE_OPEN_URL";
pub const MSG_EDGE_OPEN_URL_RESPONSE: &str = "EDGE_OPEN_URL_RESPONSE";
pub const MSG_EDGE_CLOSE_URL: &str = "EDGE_CLOSE_URL";
pub const MSG_EDGE_CLOSE_URL_RESPONSE: &str = "EDGE_CLOSE_URL_RESPONSE";
pub const MSG_EDGE_LIST_URLS: &str = "EDGE_LIST_URLS";
pub const MSG_EDGE_LIST_URLS_RESPONSE: &str = "EDGE_LIST_URLS_RESPONSE";
pub const MSG_EDGE_PUBLISH_BLOB: &str = "EDGE_PUBLISH_BLOB";
pub const MSG_EDGE_PUBLISH_BLOB_RESPONSE: &str = "EDGE_PUBLISH_BLOB_RESPONSE";
pub const MSG_EDGE_UNPUBLISH_BLOB: &str = "EDGE_UNPUBLISH_BLOB";
pub const MSG_EDGE_UNPUBLISH_BLOB_RESPONSE: &str = "EDGE_UNPUBLISH_BLOB_RESPONSE";

// IO.blob curator worker plane (SY.admin -> IO.blob). These commands never publish a URL and
// carry no external bytes: admin has already authorized the producer and resolved its tenant;
// IO.blob only materializes/releases the curated local public copy and reports status.
pub const MSG_BLOB_CURATE: &str = "BLOB_CURATE";
pub const MSG_BLOB_CURATE_RESPONSE: &str = "BLOB_CURATE_RESPONSE";
pub const MSG_BLOB_RELEASE: &str = "BLOB_RELEASE";
pub const MSG_BLOB_RELEASE_RESPONSE: &str = "BLOB_RELEASE_RESPONSE";
pub const MSG_BLOB_STATUS_GET: &str = "BLOB_STATUS_GET";
pub const MSG_BLOB_STATUS_GET_RESPONSE: &str = "BLOB_STATUS_GET_RESPONSE";

pub const SCOPE_VPN: &str = "vpn";
pub const SCOPE_GLOBAL: &str = "global";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaReloadPayload {
    pub version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

pub fn build_system_message(
    src: &str,
    dst: Destination,
    ttl: u8,
    trace_id: &str,
    msg: &str,
    payload: Value,
) -> Message {
    Message {
        routing: Routing {
            src: src.to_string(),
            src_l2_name: None,
            dst,
            ttl,
            trace_id: trace_id.to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg.to_string()),
            src_ilk: None,
            dst_ilk: None,
            ich: None,
            thread_id: None,
            thread_seq: None,
            ctx: None,
            ctx_seq: None,
            ctx_window: None,
            memory_package: None,
            scope: None,
            target: None,
            action: None,
            action_class: None,
            action_result: None,
            result_origin: None,
            result_detail_code: None,
            priority: None,
            context: None,
            via_tap: false,
        },
        payload,
    }
}

pub fn build_hello(src: &str, trace_id: &str, payload: NodeHelloPayload) -> Message {
    build_system_message(
        src,
        Destination::Resolve,
        1,
        trace_id,
        MSG_HELLO,
        json!(payload),
    )
}

pub fn build_router_hello(src: &str, trace_id: &str, payload: RouterHelloPayload) -> Message {
    build_system_message(
        src,
        Destination::Resolve,
        1,
        trace_id,
        MSG_HELLO,
        json!(payload),
    )
}

pub fn build_wan_hello(src: &str, trace_id: &str, payload: WanHelloPayload) -> Message {
    build_system_message(
        src,
        Destination::Resolve,
        1,
        trace_id,
        MSG_HELLO,
        json!(payload),
    )
}

pub fn build_wan_accept(
    src: &str,
    dst: &str,
    trace_id: &str,
    payload: WanAcceptPayload,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_WAN_ACCEPT,
        json!(payload),
    )
}

pub fn build_wan_reject(
    src: &str,
    dst: &str,
    trace_id: &str,
    payload: WanRejectPayload,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_WAN_REJECT,
        json!(payload),
    )
}

pub fn build_announce(
    src: &str,
    dst: &str,
    trace_id: &str,
    payload: NodeAnnouncePayload,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_ANNOUNCE,
        json!(payload),
    )
}

pub fn build_unreachable(
    src: &str,
    dst: &str,
    trace_id: &str,
    original_dst: &str,
    reason: &str,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        16,
        trace_id,
        MSG_UNREACHABLE,
        json!(UnreachablePayload {
            original_dst: original_dst.to_string(),
            reason: reason.to_string(),
        }),
    )
}

pub fn build_ttl_exceeded(
    src: &str,
    dst: &str,
    trace_id: &str,
    original_dst: &str,
    last_hop: &str,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        16,
        trace_id,
        MSG_TTL_EXCEEDED,
        json!(TtlExceededPayload {
            original_dst: original_dst.to_string(),
            last_hop: last_hop.to_string(),
        }),
    )
}

pub fn build_lsa(src: &str, dst: &str, trace_id: &str, payload: LsaPayload) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_LSA,
        json!(payload),
    )
}

/// Build a hub-authored `MSG_WAN_REACHABILITY` advertisement (ttl=1, single WAN hop to the peer).
pub fn build_wan_reachability(
    src: &str,
    dst: &str,
    trace_id: &str,
    payload: WanReachabilityPayload,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_WAN_REACHABILITY,
        json!(payload),
    )
}

pub fn build_echo(src: &str, dst: Destination, trace_id: &str) -> Message {
    build_system_message(src, dst, 1, trace_id, MSG_ECHO, json!(EchoPayload {}))
}

pub fn build_echo_reply(src: &str, dst: Destination, trace_id: &str) -> Message {
    build_system_message(
        src,
        dst,
        1,
        trace_id,
        MSG_ECHO_REPLY,
        json!(EchoReplyPayload {}),
    )
}

pub fn build_time_sync(
    src: &str,
    dst: Destination,
    trace_id: &str,
    payload: TimeSyncPayload,
) -> Message {
    let mut msg = build_system_message(src, dst, 1, trace_id, MSG_TIME_SYNC, json!(payload));
    msg.meta.scope = Some(SCOPE_GLOBAL.to_string());
    msg
}

pub fn build_withdraw(src: &str, dst: Destination, trace_id: &str, uuid: &str) -> Message {
    build_system_message(
        src,
        dst,
        1,
        trace_id,
        MSG_WITHDRAW,
        json!(WithdrawPayload {
            uuid: uuid.to_string(),
        }),
    )
}

fn deserialize_dst<'de, D>(deserializer: D) -> Result<Destination, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(Destination::Resolve),
        Value::String(s) if s == "broadcast" => Ok(Destination::Broadcast),
        Value::String(s) => Ok(Destination::Unicast(s)),
        _ => Err(serde::de::Error::custom("invalid dst")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_system_message_omits_src_l2_name_on_outbound_messages() {
        let msg = build_system_message(
            "src-uuid-1",
            Destination::Unicast("dst-uuid-1".to_string()),
            16,
            "trace-1",
            MSG_ECHO,
            json!({}),
        );
        assert_eq!(msg.routing.src_l2_name, None);

        let encoded = serde_json::to_value(&msg).expect("serialize");
        let routing = encoded
            .get("routing")
            .and_then(Value::as_object)
            .expect("routing object");
        assert!(!routing.contains_key("src_l2_name"));
    }

    #[test]
    fn routing_parses_router_stamped_src_l2_name() {
        let raw = json!({
            "routing": {
                "src": "src-uuid-1",
                "src_l2_name": "WF.demo@motherbee",
                "dst": "dst-uuid-1",
                "ttl": 16,
                "trace_id": "trace-1"
            },
            "meta": {
                "type": "system",
                "msg": "HELLO"
            },
            "payload": {}
        });

        let msg: Message = serde_json::from_value(raw).expect("deserialize message");
        assert_eq!(
            msg.routing.src_l2_name.as_deref(),
            Some("WF.demo@motherbee")
        );
    }

    // L2-LOOKUP-10: source_l2_name() helper
    #[test]
    fn source_l2_name_returns_stamped_value() {
        let raw = json!({
            "routing": {
                "src": "abc-uuid",
                "src_l2_name": "AI.chat@motherbee",
                "dst": "dst-uuid",
                "ttl": 16,
                "trace_id": "trace-x"
            },
            "meta": { "type": "system", "msg": "ECHO" },
            "payload": {}
        });
        let msg: Message = serde_json::from_value(raw).expect("deserialize");
        assert_eq!(msg.source_l2_name(), Some("AI.chat@motherbee"));
    }

    #[test]
    fn source_l2_name_returns_none_when_absent() {
        let msg = build_system_message(
            "abc-uuid",
            Destination::Unicast("dst-uuid".to_string()),
            16,
            "trace-x",
            MSG_ECHO,
            json!({}),
        );
        assert_eq!(msg.source_l2_name(), None);
    }

    // L2-LOOKUP-12: wire compatibility — src_l2_name absent in serialized output when None
    #[test]
    fn serialize_roundtrip_without_src_l2_name() {
        let msg = build_system_message(
            "uuid-sender",
            Destination::Unicast("uuid-dst".to_string()),
            8,
            "trace-rt",
            MSG_ECHO,
            json!({"key": "value"}),
        );
        let encoded = serde_json::to_string(&msg).expect("serialize");
        assert!(
            !encoded.contains("src_l2_name"),
            "field must be absent when None"
        );

        let decoded: Message = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded.routing.src, "uuid-sender");
        assert_eq!(decoded.routing.src_l2_name, None);
        assert_eq!(decoded.routing.ttl, 8);
    }

    #[test]
    fn serialize_roundtrip_with_src_l2_name() {
        let raw = json!({
            "routing": {
                "src": "uuid-sender",
                "src_l2_name": "IO.webchat@hivename",
                "dst": "uuid-dst",
                "ttl": 8,
                "trace_id": "trace-rt2"
            },
            "meta": { "type": "system", "msg": "ECHO" },
            "payload": {}
        });
        let msg: Message = serde_json::from_value(raw).expect("deserialize");
        assert_eq!(
            msg.routing.src_l2_name.as_deref(),
            Some("IO.webchat@hivename")
        );

        let re_encoded = serde_json::to_value(&msg).expect("re-serialize");
        let re_field = re_encoded["routing"]["src_l2_name"].as_str();
        assert_eq!(re_field, Some("IO.webchat@hivename"));
    }

    #[test]
    fn messages_without_src_l2_name_field_deserialize_cleanly() {
        // Wire messages from older nodes or the router itself (UNREACHABLE, HELLO) will
        // never carry src_l2_name — they must still parse without errors.
        let raw = json!({
            "routing": {
                "src": "uuid-router",
                "dst": "uuid-dst",
                "ttl": 16,
                "trace_id": "trace-old"
            },
            "meta": { "type": "system", "msg": "UNREACHABLE" },
            "payload": { "original_dst": "uuid-x", "reason": "NODE_NOT_FOUND" }
        });
        let msg: Message = serde_json::from_value(raw).expect("deserialize");
        assert_eq!(msg.routing.src_l2_name, None);
        assert_eq!(msg.source_l2_name(), None);
    }

    fn vault_changed_event(
        resource_type: &str,
        tenant_id: &str,
        ilk: Option<&str>,
    ) -> VaultSecretChangedPayload {
        VaultSecretChangedPayload {
            op: VaultSecretOp::Put,
            resource_type: resource_type.to_string(),
            tenant_id: tenant_id.to_string(),
            ilk: ilk.map(str::to_string),
            version: 1,
            key: "infra:test".to_string(),
            hive_id: "motherbee".to_string(),
            at_ms: 1,
        }
    }

    #[test]
    fn vault_secret_changed_matches_dedicated_owner_only() {
        let event = vault_changed_event(
            "openai",
            crate::identity::DEFAULT_ROOT_TENANT_ID,
            Some("ilk:owner"),
        );

        assert!(event.matches_interest(&VaultSecretInterest {
            resource_type: "openai",
            my_tenant: "tnt:client",
            my_ilk: Some("ilk:owner"),
            system_caller: false,
        }));
        assert!(!event.matches_interest(&VaultSecretInterest {
            resource_type: "openai",
            my_tenant: crate::identity::DEFAULT_ROOT_TENANT_ID,
            my_ilk: Some("ilk:other"),
            system_caller: true,
        }));
    }

    #[test]
    fn vault_secret_changed_matches_same_tenant_pool() {
        let event = vault_changed_event("slack", "tnt:client", None);

        assert!(event.matches_interest(&VaultSecretInterest {
            resource_type: "slack",
            my_tenant: "tnt:client",
            my_ilk: Some("ilk:consumer"),
            system_caller: false,
        }));
        assert!(!event.matches_interest(&VaultSecretInterest {
            resource_type: "slack",
            my_tenant: "tnt:other",
            my_ilk: Some("ilk:consumer"),
            system_caller: false,
        }));
    }

    #[test]
    fn vault_secret_changed_matches_root_pool_for_system_callers_only() {
        let event = vault_changed_event("postgres", crate::identity::DEFAULT_ROOT_TENANT_ID, None);

        assert!(event.matches_interest(&VaultSecretInterest {
            resource_type: "postgres",
            my_tenant: "tnt:client",
            my_ilk: Some("ilk:sy-storage"),
            system_caller: true,
        }));
        assert!(!event.matches_interest(&VaultSecretInterest {
            resource_type: "postgres",
            my_tenant: "tnt:client",
            my_ilk: Some("ilk:ai-sales"),
            system_caller: false,
        }));
    }

    #[test]
    fn vault_secret_changed_rejects_resource_type_mismatch() {
        let event = vault_changed_event("openai", crate::identity::DEFAULT_ROOT_TENANT_ID, None);

        assert!(!event.matches_interest(&VaultSecretInterest {
            resource_type: "postgres",
            my_tenant: crate::identity::DEFAULT_ROOT_TENANT_ID,
            my_ilk: Some("ilk:sy-storage"),
            system_caller: true,
        }));
    }
}
