use std::fmt;
use std::net::IpAddr;

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

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// The `MemoryPackage` SY.cognition writes: 3 added `turns` (2026-10-30), 4 made `episodes` a
/// person's feelings (2026-11-05), 5 named them `feelings` and added `news`, the episodes of the
/// communication's people (2026-11-06).
pub const MEMORY_PACKAGE_VERSION: u32 = 5;

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
    /// The people's feelings in this communication (docs/cognition-hands-v1.md §10.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feelings: Vec<FeelingSummary>,
    /// The episodes of the communication's people that still rank, the strongest first
    /// (docs/cognition-hands-v1.md §12.4): what happened between people, wherever it happened. The
    /// agent takes them into account to answer, and never passes them on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub news: Vec<NewsSummary>,
    /// The communication's last turns, oldest first, as SY.cognition took them when they arrived.
    /// The message the package rides on is not among them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<MemoryTurn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<MemoryPackageTruncated>,
}

/// One turn of a communication: a message its agent received (`in`) or sent (`out`). A relay, what
/// a WF sent an IO node on the agent's thread, is an `in` that names where it went (`to_via`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTurn {
    pub direction: MemoryTurnDirection,
    /// The sender's ILK, when the message carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_ilk: Option<String>,
    /// Only on `in`: the node it came in through, where its cycle started: its trace's first entry
    /// (docs/message-trace-v1.md), the IO node a WF forwards it from; otherwise its sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Only on `in`: who wrote it, its `meta.context.writer`, else its `src_ilk`
    /// (docs/cognition-hands-v1.md §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer: Option<MemoryTurnParty>,
    /// On `out`: whom the agent answered, the answer's `dst_ilk`. On a relay: the person it went
    /// to, when the message names them, its `dst_ilk` (docs/cognition-hands-v1.md §12.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<MemoryTurnParty>,
    /// Only on a relay: the IO node the WF sent it to. It was said to whoever is there, not to the
    /// agent (docs/cognition-hands-v1.md §12.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_via: Option<String>,
    /// Its text, cut at 2,000 characters. Empty when it carried only files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// How many files it carried; a text sent as a file (`content_ref`) counts as one.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub attachments: u32,
    /// Only on `out`: what the agent chose from a WF's menu with this answer. The turn is kept for
    /// it even when the answer's text is blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<MemoryTurnChoice>,
    /// When its agent's router took it (RFC 3339, UTC).
    pub ts: String,
}

/// Who is at one end of a turn (docs/cognition-hands-v1.md §6): their ILK, their name, and their
/// kind, as the message and the identity memory say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTurnParty {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `person`, `agent`, `workflow` or `system` (`message_writer::IlkFacts::kind`); none when the
    /// identity does not know the ILK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// An option an agent chose from a WF's menu, as its answer's `meta.context.evaluation` names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTurnChoice {
    pub option: String,
    /// Its params as compact JSON, cut at 2,000 characters; empty when it has none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub params: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTurnDirection {
    /// The agent received it.
    In,
    /// The agent sent it.
    Out,
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

/// A person's feeling, as the package carries it (docs/cognition-hands-v1.md §10.3): whose it is,
/// what and about what, how strong it is now, when its owner last showed it, and its text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeelingSummary {
    pub feeling_id: String,
    /// The person whose turns show it.
    pub owner: MemoryTurnParty,
    /// `anger`, `escalation` or `urgency`.
    pub affect: String,
    /// The context it was felt around.
    pub context: String,
    pub title: String,
    /// How strong it is now: `strong`, `present` or `faint`.
    pub level: String,
    pub weight: f64,
    /// When its owner last showed it (RFC 3339, UTC).
    pub last_felt_at: String,
    /// The narrator's text of it, cut at 300 characters; empty when it has none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

/// An episode, as the package carries it (docs/cognition-hands-v1.md §12.4): a social event
/// between people, public like the news, which belongs to the people it is about and fades with
/// time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsSummary {
    pub episode_id: String,
    /// `friction`, `discomfort`, `appreciation` or `shame`.
    pub lens: String,
    pub title: String,
    /// A paragraph: what happened, between whom, and how it stood.
    pub summary: String,
    /// `resolved`, `pending`, `follow_up` or `inconclusive`.
    pub state: String,
    /// The people it is about.
    pub people: Vec<MemoryTurnParty>,
    /// When it happened (RFC 3339, UTC).
    pub born_at: String,
    /// Its weight when the package was written, out of 10: its intensity, halved every 7 days.
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPackageTruncated {
    pub applied: bool,
    pub dropped_contexts: u32,
    pub dropped_reasons: u32,
    pub dropped_memories: u32,
    #[serde(default)]
    pub dropped_feelings: u32,
    #[serde(default)]
    pub dropped_news: u32,
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
    /// The hive this config belongs to. `None` = every hive (global config). `Some(h)` =
    /// hive-scoped config (e.g. the route list of hive `h` after `add_route?hive=h`): a
    /// receiver on any other hive MUST ignore it, or it would overwrite its own config
    /// with another hive's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive: Option<String>,
}

impl ConfigChangedPayload {
    /// True when this payload may be applied by a receiver on `local_hive`.
    pub fn addressed_to(&self, local_hive: &str) -> bool {
        match self.hive.as_deref().map(str::trim) {
            None | Some("") => true,
            Some(hive) => hive == local_hive,
        }
    }
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
    /// Owner ILK when the secret has one (a node or a person). `None` (or empty string) when it
    /// has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk: Option<String>,
    /// The nodes the secret is assigned to; on an assignment change, those it had and those it
    /// has now, so a node that lost it reacts too (docs/ilk-tenant-model-v1.md §2.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assigned_ilks: Vec<String>,
    /// The secret is, or was before this change, its tenant's general secret (no owner, no
    /// assigned node): every node of the tenant may have been reading it.
    #[serde(default)]
    pub general: bool,
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
    /// The nodes it is assigned to changed; the value did not.
    Assign,
}

impl VaultSecretOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Rotate => "rotate",
            Self::Delete => "delete",
            Self::Rollback => "rollback",
            Self::Assign => "assign",
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
        // (1) Owned by me, or assigned to me (now or before this change).
        if let Some(mine) = my_ilk {
            if secret_ilk == Some(mine) || self.assigned_ilks.iter().any(|ilk| ilk.trim() == mine) {
                return true;
            }
        }
        // (2) General — no owner and no assigned node, now or before this change.
        if self.general {
            // (2a) My tenant's general secret.
            if self.tenant_id == interest.my_tenant {
                return true;
            }
            // (2b) The genesis (root) tenant's general secrets are the system's keys: any SY
            // system caller reads them.
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

/// Whether a message-kind string is the solution's: any kind but system, admin and
/// query_response. The router publishes these as turns and gives an agent its memory with them, and
/// they carry the trace of their cycle (docs/message-trace-v1.md).
#[inline]
pub fn is_solution_kind(msg_type: &str) -> bool {
    !is_system_kind(msg_type) && msg_type != "admin" && msg_type != "query_response"
}

pub const MSG_HELLO: &str = "HELLO";
pub const MSG_ANNOUNCE: &str = "ANNOUNCE";
pub const MSG_UNREACHABLE: &str = "UNREACHABLE";
pub const MSG_TTL_EXCEEDED: &str = "TTL_EXCEEDED";
pub const MSG_LSA: &str = "LSA";
/// Hub-authored multi-hop reachability advertisement (Option B, edge-multihop-reachability-spec-v1).
/// Emitted ONLY by a gateway (hub) in its own authenticated bucket, listing spoke nodes reachable
/// through it so a spoke can resolve+forward to a non-adjacent hive. It is DISTINCT from `LSA`: it
/// never writes the identity-bearing LSA snapshot and its entries are treated as `via_hub`
/// (transitively learned) — admitted for data delivery but denied SYSTEM authority.
pub const MSG_WAN_REACHABILITY: &str = "WAN_REACHABILITY";
pub const MSG_WAN_ACCEPT: &str = "WAN_ACCEPT";
pub const MSG_WAN_REJECT: &str = "WAN_REJECT";
pub const MSG_WITHDRAW: &str = "WITHDRAW";
pub const MSG_CONFIG_CHANGED: &str = "CONFIG_CHANGED";
/// A node's answer to a message it took nowhere: a `system` message to the sender's `routing.src`,
/// on the message's `trace_id` and `meta.thread_id`, with the payload
/// `{"ok": false, "error": "not_taken", "reason": "...", "detail": "...", "retryable": ...}`.
/// Nothing of the message ran. The WF engine sends it since 0.1.94 with its own reasons
/// (docs/wf-orchestration-v1.md §5.4.6). The SDKs send it for a message their command channel has
/// no room for, with reason [`NOT_TAKEN_REASON_NODE_BUSY`] and `retryable: true`: the Go SDK since
/// 0.1.141 (FINDINGS A-329), the Rust SDK since 0.1.143 (planned; A-295). It is a reply: never
/// answered.
pub const MSG_NOT_TAKEN: &str = "NOT_TAKEN";
/// `NOT_TAKEN`'s reason when the receiving node had no room for the message (its command channel's
/// lane held as many messages or bytes as it takes): sending it again later may pass. The Go SDK's
/// `NotTakenReasonNodeBusy`.
pub const NOT_TAKEN_REASON_NODE_BUSY: &str = "node_busy";

/// The primary hive — the single motherbee, home of the singleton SY.admin and SY.vault.
/// Nodes on any other hive reach those services here, never at their own hive.
pub const PRIMARY_HIVE_ID: &str = "motherbee";
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
/// A node's self-description (docs/agent-tools-v1.md T3, docs/sy-timer.md §3): an AI node answers
/// with its tool catalog. It travels as `NODE_STATUS_GET` does, SY.admin → the node's hive
/// orchestrator → the node.
pub const MSG_NODE_HELP: &str = "NODE_HELP";
pub const MSG_NODE_HELP_RESPONSE: &str = "NODE_HELP_RESPONSE";

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
// The edge's own certificate (SY.admin -> SY.edge; docs/acme-edge-certificates-v1.md §4): what it
// serves, a reload from the vault, and the TLS-ALPN-01 challenge it answers while SY.admin orders.
pub const MSG_EDGE_TLS_STATUS: &str = "EDGE_TLS_STATUS";
pub const MSG_EDGE_TLS_STATUS_RESPONSE: &str = "EDGE_TLS_STATUS_RESPONSE";
pub const MSG_EDGE_TLS_RELOAD: &str = "EDGE_TLS_RELOAD";
pub const MSG_EDGE_TLS_RELOAD_RESPONSE: &str = "EDGE_TLS_RELOAD_RESPONSE";
pub const MSG_EDGE_ACME_CHALLENGE: &str = "EDGE_ACME_CHALLENGE";
pub const MSG_EDGE_ACME_CHALLENGE_RESPONSE: &str = "EDGE_ACME_CHALLENGE_RESPONSE";
// The edge's web hosts (SY.admin -> SY.edge; docs/io-web-v2.md §11): the full set of hosts the
// edge serves only through io.web, never through `/e/` or `/public/`. SY.admin pushes it every
// minute; the edge keeps it on disk.
pub const MSG_EDGE_WEB_ROUTES: &str = "EDGE_WEB_ROUTES";
pub const MSG_EDGE_WEB_ROUTES_RESPONSE: &str = "EDGE_WEB_ROUTES_RESPONSE";
// io.web's exposures (SY.admin -> IO.web; docs/io-web-v2-stage-2-1.md §5.5): the whole set of
// exposures with their live enablements (`IoWebExposuresPayload`), and a one-time link to an
// exposure's current enablement (`IoWebAccessLinkRequest`). Only `SY.admin@motherbee` sends them,
// as system messages; neither carries a secret, since io.web reads each enablement's key from the
// vault itself (`vault::web_exposure_vault_key`).
pub const MSG_IO_WEB_EXPOSURES: &str = "IO_WEB_EXPOSURES";
pub const MSG_IO_WEB_EXPOSURES_RESPONSE: &str = "IO_WEB_EXPOSURES_RESPONSE";
pub const MSG_IO_WEB_ACCESS_LINK: &str = "IO_WEB_ACCESS_LINK";
pub const MSG_IO_WEB_ACCESS_LINK_RESPONSE: &str = "IO_WEB_ACCESS_LINK_RESPONSE";

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

// ---------------------------------------------------------------------------
// io.web's exposures (docs/io-web-v2-stage-2-1.md): what SY.admin pushes to io.web
// (`MSG_IO_WEB_EXPOSURES`) and asks of it (`MSG_IO_WEB_ACCESS_LINK`), and their answers. SY.admin
// and io.web ship in different cargo workspaces, so their shapes live here.
// ---------------------------------------------------------------------------

/// The prefix of an exposure's id, `exp:<uuid v4>`, which SY.admin mints at publish and never
/// reuses (docs/io-web-v2-stage-2-1.md §2.1).
pub const WEB_EXPOSURE_ID_PREFIX: &str = "exp:";
/// The prefix of an enablement's id, `enb:<uuid v4>`, which SY.admin mints at each enable (§2.3).
pub const WEB_ENABLEMENT_ID_PREFIX: &str = "enb:";

/// io.web's answer to a [`MSG_IO_WEB_EXPOSURES`] or a [`MSG_IO_WEB_ACCESS_LINK`] from anyone but
/// `SY.admin@motherbee`, or that came as a tap copy: nothing was read (§5.5).
pub const IO_WEB_ERROR_UNAUTHORIZED: &str = "UNAUTHORIZED";
/// A pushed set whose generation is lower than the one io.web holds: io.web keeps its own (§5.5).
pub const IO_WEB_ERROR_STALE_GENERATION: &str = "STALE_GENERATION";
/// A link asked for an exposure io.web's set does not hold (§5.5).
pub const IO_WEB_ERROR_EXPOSURE_UNKNOWN: &str = "EXPOSURE_UNKNOWN";
/// A link asked for an enablement that is not the exposure's current one in io.web's set. SY.admin
/// then pushes the set and asks once more (§5.1).
pub const IO_WEB_ERROR_ENABLEMENT_UNKNOWN: &str = "ENABLEMENT_UNKNOWN";
/// A link asked for the current enablement while io.web does not hold its key. SY.admin answers
/// `IO_WEB_UNAVAILABLE`, retryable (§5.1).
pub const IO_WEB_ERROR_KEY_UNAVAILABLE: &str = "KEY_UNAVAILABLE";

/// [`MSG_IO_WEB_EXPOSURES`]: the whole set of exposures, which replaces io.web's (§2.3, §5.5).
/// SY.admin pushes it at each change and every 60 s. It holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoWebExposuresPayload {
    /// `max(previous + 1, now in ms)`, so it never goes back, even when SY.admin's file is lost
    /// (§2.3). io.web refuses a lower one than it holds ([`IO_WEB_ERROR_STALE_GENERATION`]) and
    /// acknowledges an equal one without reading anything.
    pub generation: u64,
    /// Every published exposure, the closed ones included.
    pub exposures: Vec<IoWebExposure>,
}

/// One exposure as io.web holds it (§2.3): one host to one fixed upstream, with its live
/// enablements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoWebExposure {
    /// `exp:<uuid v4>` ([`WEB_EXPOSURE_ID_PREFIX`]).
    pub exposure_id: String,
    /// `<label>.<main>`: the host its requests come with (§2.1).
    pub host: String,
    pub upstream: WebExposureUpstream,
    /// Its live enablements, newest last: at most two, the current one and the one it renewed,
    /// whose sessions run to that one's own end (§2.3, §3.4). None: the exposure is closed.
    pub enablements: Vec<IoWebEnablement>,
}

/// Where an exposure leads (§2.2): recorded in stage 2.1, dialled in 2.2. SY.admin checks it at
/// publish, and never takes one of Fluxbee's own services or ports. Each kind takes only its own
/// fields: any other is refused, never ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WebExposureUpstream {
    /// Archi on the motherbee's loopback, whose address is io.web's own constant: `{"kind":
    /// "archi"}`, no field.
    Archi {},
    /// A worker of the hive registry, by its hive id, at the registry's address.
    Hive { hive: String, port: u16 },
    /// An IP literal in a private range, never a name.
    Address { address: IpAddr, port: u16 },
}

/// A live enablement as io.web holds it (§2.3), never its key, which io.web reads from the vault
/// itself (`vault::web_exposure_vault_key`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoWebEnablement {
    /// `enb:<uuid v4>` ([`WEB_ENABLEMENT_ID_PREFIX`]).
    pub enablement_id: String,
    /// Its end, in Unix seconds: io.web's own clock closes it then (§3.4).
    pub until: u64,
    /// The vault slot SY.admin put its key in.
    pub slot: WebExposureKeySlot,
    /// The version of the slot's key SY.admin wrote for it. io.web takes the key at this version
    /// only (§3.1).
    pub key_version: i64,
}

/// One of an exposure's two key slots (§2.3). Each enable writes the slot the current enablement
/// does not use, so a put that fails or goes unanswered never touches a live enablement's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebExposureKeySlot {
    A,
    B,
}

impl WebExposureKeySlot {
    /// `a` or `b`, as the payloads and the vault key name the slot.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

/// [`MSG_IO_WEB_EXPOSURES_RESPONSE`] (§5.5): `{"status": "ok", generation, changed, enablements}`,
/// or `{"status": "error", error_code, error_detail}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IoWebExposuresResponse {
    /// io.web holds the set pushed, or held it already.
    Ok {
        /// The generation io.web holds now.
        generation: u64,
        /// The set differs from the one io.web held.
        changed: bool,
        /// Each live enablement of the set, and whether io.web holds its key.
        enablements: Vec<IoWebEnablementKey>,
    },
    /// The set was not taken: [`IO_WEB_ERROR_UNAUTHORIZED`], [`IO_WEB_ERROR_STALE_GENERATION`],
    /// or a payload io.web could not read.
    Error {
        error_code: String,
        #[serde(default)]
        error_detail: String,
    },
}

/// Whether io.web holds an enablement's key (§5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoWebEnablementKey {
    pub enablement_id: String,
    pub key: IoWebKeyState,
    /// Why the key is unavailable: the vault's answer, another version or another resource type.
    /// Never the key. `None` when it is ready.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoWebKeyState {
    /// io.web holds the key at the enablement's version: its links and sessions work.
    Ready,
    /// io.web could not read the key, or it did not match: the enablement fails closed, and io.web
    /// reads it again every 30 s.
    Unavailable,
}

/// [`MSG_IO_WEB_ACCESS_LINK`] (§5.1, §5.5): a one-time link to an exposure's current enablement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoWebAccessLinkRequest {
    pub exposure_id: String,
    pub enablement_id: String,
    /// The generation of SY.admin's set when it asked. While io.web's set is older it may not know
    /// the enablement yet ([`IO_WEB_ERROR_ENABLEMENT_UNKNOWN`]).
    pub generation: u64,
}

/// [`MSG_IO_WEB_ACCESS_LINK_RESPONSE`] (§3.2, §5.5): `{"status": "ok", url, expires_at, link_id}`,
/// or `{"status": "error", error_code, error_detail}`. Its `Debug` leaves the link out: the link
/// holds the one-time code, which no log holds.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IoWebAccessLinkResponse {
    Ok {
        /// `https://<host>/.fluxbee/enter?code=…`, opened once, top-level, before `expires_at`.
        url: String,
        /// When the code stops being taken, in Unix seconds: about 120 s after io.web minted it.
        expires_at: u64,
        /// What the logs name the link by: the first 16 hex of the SHA-256 of its `jti`.
        link_id: String,
    },
    /// No link: [`IO_WEB_ERROR_EXPOSURE_UNKNOWN`], [`IO_WEB_ERROR_ENABLEMENT_UNKNOWN`],
    /// [`IO_WEB_ERROR_KEY_UNAVAILABLE`] or [`IO_WEB_ERROR_UNAUTHORIZED`].
    Error {
        error_code: String,
        #[serde(default)]
        error_detail: String,
    },
}

impl fmt::Debug for IoWebAccessLinkResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok {
                url: _,
                expires_at,
                link_id,
            } => f
                .debug_struct("Ok")
                .field("url", &crate::node_secret::NODE_SECRET_REDACTION_TOKEN)
                .field("expires_at", expires_at)
                .field("link_id", link_id)
                .finish(),
            Self::Error {
                error_code,
                error_detail,
            } => f
                .debug_struct("Error")
                .field("error_code", error_code)
                .field("error_detail", error_detail)
                .finish(),
        }
    }
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

    /// A package's `truncated` from a router or a cognition of the version before reads the counts
    /// it lacks as none, so an upgrade never fails a whole region or message over them.
    #[test]
    fn a_truncated_without_its_newer_counts_reads_them_as_none() {
        let truncated: MemoryPackageTruncated = serde_json::from_value(json!({
            "applied": true, "dropped_contexts": 1, "dropped_reasons": 0, "dropped_memories": 2
        }))
        .expect("parse");
        assert_eq!(truncated.dropped_memories, 2);
        assert_eq!(truncated.dropped_feelings, 0);
        assert_eq!(truncated.dropped_news, 0);
    }

    /// A relay names the IO node it went to, and a turn of the version before, with none, reads
    /// as a turn said to the agent (docs/cognition-hands-v1.md §12.9).
    #[test]
    fn a_relay_names_where_it_went_and_an_older_turn_reads_without_it() {
        let relay: MemoryTurn = serde_json::from_value(json!({
            "direction": "in", "via": "IO.a@worker1", "to_via": "IO.b@worker1",
            "text": "hello", "ts": "2026-11-07T05:40:47.120Z"
        }))
        .expect("parse");
        assert_eq!(relay.to_via.as_deref(), Some("IO.b@worker1"));
        assert_eq!(
            serde_json::to_value(&relay).expect("encode")["to_via"],
            "IO.b@worker1"
        );
        let older: MemoryTurn = serde_json::from_value(json!({
            "direction": "in", "via": "IO.a@worker1", "text": "hello", "ts": "2026-11-07T05:40:47.120Z"
        }))
        .expect("parse");
        assert_eq!(older.to_via, None);
        assert!(serde_json::to_value(&older)
            .expect("encode")
            .get("to_via")
            .is_none());
    }

    #[test]
    fn config_changed_payload_hive_scope() {
        let global: ConfigChangedPayload =
            serde_json::from_value(json!({"subsystem": "routes", "version": 3})).expect("parse");
        assert_eq!(global.hive, None);
        assert!(global.addressed_to("motherbee"));
        assert!(global.addressed_to("worker1"));
        assert!(serde_json::to_value(&global)
            .expect("encode")
            .get("hive")
            .is_none());

        let scoped: ConfigChangedPayload =
            serde_json::from_value(json!({"subsystem": "routes", "version": 3, "hive": "worker1"}))
                .expect("parse");
        assert!(scoped.addressed_to("worker1"));
        assert!(!scoped.addressed_to("motherbee"));

        let blank: ConfigChangedPayload =
            serde_json::from_value(json!({"subsystem": "routes", "version": 3, "hive": " "}))
                .expect("parse");
        assert!(blank.addressed_to("motherbee"));
    }

    #[test]
    fn build_system_message_omits_src_l2_name_on_outbound_messages() {
        let msg = build_system_message(
            "src-uuid-1",
            Destination::Unicast("dst-uuid-1".to_string()),
            16,
            "trace-1",
            MSG_CONFIG_GET,
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
            "meta": { "type": "system", "msg": "CONFIG_GET" },
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
            MSG_CONFIG_GET,
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
            MSG_CONFIG_GET,
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
            "meta": { "type": "system", "msg": "CONFIG_GET" },
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
            assigned_ilks: Vec::new(),
            general: ilk.is_none(),
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
    fn vault_secret_changed_reaches_assigned_nodes_and_the_tenant_only_when_general() {
        let interest = |ilk: &'static str| VaultSecretInterest {
            resource_type: "slack",
            my_tenant: "tnt:client",
            my_ilk: Some(ilk),
            system_caller: false,
        };
        let mut event = vault_changed_event("slack", "tnt:client", None);
        event.op = VaultSecretOp::Assign;
        event.assigned_ilks = vec!["ilk:sales".to_string(), "ilk:support".to_string()];
        event.general = false;
        assert!(event.matches_interest(&interest("ilk:sales")));
        assert!(event.matches_interest(&interest("ilk:support")));
        // Another node of the tenant: the secret is not general.
        assert!(!event.matches_interest(&interest("ilk:billing")));
        // It was general before the assignment: every node of the tenant may have been reading it.
        event.general = true;
        assert!(event.matches_interest(&interest("ilk:billing")));
        // A person-owned secret reaches its assigned node, not the person's other nodes.
        let mut owned = vault_changed_event("slack", "tnt:client", Some("ilk:person"));
        owned.assigned_ilks = vec!["ilk:agent".to_string()];
        assert!(owned.matches_interest(&interest("ilk:agent")));
        assert!(!owned.matches_interest(&interest("ilk:billing")));
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

    /// Sends `payload` as `msg` from SY.admin to io.web, through the wire, and reads it back as `T`.
    fn through_the_wire<T>(msg: &str, payload: &T) -> (String, T)
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let sent = build_system_message(
            "admin-uuid",
            Destination::Unicast("IO.web@motherbee".to_string()),
            16,
            "trace-web",
            msg,
            serde_json::to_value(payload).expect("encode payload"),
        );
        let wire = serde_json::to_string(&sent).expect("encode message");
        let got: Message = serde_json::from_str(&wire).expect("decode message");
        assert_eq!(got.meta.msg_type, SYSTEM_KIND);
        (
            got.meta.msg.expect("named"),
            serde_json::from_value(got.payload).expect("decode payload"),
        )
    }

    /// io.web's commands, their answers and its error codes carry the names
    /// docs/io-web-v2-stage-2-1.md §5.5 gives them, which SY.admin and io.web both compile against.
    #[test]
    fn io_web_commands_and_codes_keep_the_designs_names() {
        assert_eq!(MSG_IO_WEB_EXPOSURES, "IO_WEB_EXPOSURES");
        assert_eq!(MSG_IO_WEB_EXPOSURES_RESPONSE, "IO_WEB_EXPOSURES_RESPONSE");
        assert_eq!(MSG_IO_WEB_ACCESS_LINK, "IO_WEB_ACCESS_LINK");
        assert_eq!(
            MSG_IO_WEB_ACCESS_LINK_RESPONSE,
            "IO_WEB_ACCESS_LINK_RESPONSE"
        );
        assert_eq!(IO_WEB_ERROR_UNAUTHORIZED, "UNAUTHORIZED");
        assert_eq!(IO_WEB_ERROR_STALE_GENERATION, "STALE_GENERATION");
        assert_eq!(IO_WEB_ERROR_EXPOSURE_UNKNOWN, "EXPOSURE_UNKNOWN");
        assert_eq!(IO_WEB_ERROR_ENABLEMENT_UNKNOWN, "ENABLEMENT_UNKNOWN");
        assert_eq!(IO_WEB_ERROR_KEY_UNAVAILABLE, "KEY_UNAVAILABLE");
        assert_eq!(WEB_EXPOSURE_ID_PREFIX, "exp:");
        assert_eq!(WEB_ENABLEMENT_ID_PREFIX, "enb:");
    }

    /// `IO_WEB_EXPOSURES` reads and writes the set as §2.3 shows it, each upstream kind with its
    /// own fields, and crosses a message whole.
    #[test]
    fn io_web_exposures_round_trip_in_the_designs_shape() {
        let wire = json!({
            "generation": 1_763_121_600_123u64,
            "exposures": [
                {
                    "exposure_id": "exp:6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
                    "host": "shop.hive-k3m9x7q2.fluxbee.ai",
                    "upstream": {"kind": "hive", "hive": "worker1", "port": 8080},
                    "enablements": [
                        {"enablement_id": "enb:0a1b2c3d-4e5f-4a6b-8c7d-8e9fa0b1c2d3",
                         "until": 1_763_150_500u64, "slot": "a", "key_version": 4},
                        {"enablement_id": "enb:1b2c3d4e-5f6a-4b7c-9d8e-9fa0b1c2d3e4",
                         "until": 1_763_179_300u64, "slot": "b", "key_version": 1}
                    ]
                },
                {
                    "exposure_id": "exp:7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d",
                    "host": "archi.hive-k3m9x7q2.fluxbee.ai",
                    "upstream": {"kind": "archi"},
                    "enablements": []
                },
                {
                    "exposure_id": "exp:8b9c0d1e-2f3a-4b4c-9d5e-6f7a8b9c0d1e",
                    "host": "erp.hive-k3m9x7q2.fluxbee.ai",
                    "upstream": {"kind": "address", "address": "fc00::12", "port": 8443},
                    "enablements": []
                }
            ]
        });
        let set: IoWebExposuresPayload = serde_json::from_value(wire.clone()).expect("parse");
        assert_eq!(set.generation, 1_763_121_600_123);
        assert_eq!(
            set.exposures[0].upstream,
            WebExposureUpstream::Hive {
                hive: "worker1".to_string(),
                port: 8080
            }
        );
        let renewed = &set.exposures[0].enablements;
        assert_eq!(
            (renewed[0].slot, renewed[0].key_version),
            (WebExposureKeySlot::A, 4)
        );
        assert_eq!(
            (renewed[1].slot, renewed[1].until),
            (WebExposureKeySlot::B, 1_763_179_300)
        );
        assert_eq!(set.exposures[1].upstream, WebExposureUpstream::Archi {});
        assert_eq!(
            set.exposures[2].upstream,
            WebExposureUpstream::Address {
                address: "fc00::12".parse().expect("ip"),
                port: 8443
            }
        );
        assert_eq!(serde_json::to_value(&set).expect("encode"), wire);

        let (msg, got) = through_the_wire(MSG_IO_WEB_EXPOSURES, &set);
        assert_eq!((msg.as_str(), got), (MSG_IO_WEB_EXPOSURES, set));
    }

    /// An upstream takes only its kind's own fields, and an address only as an IP literal (§2.2):
    /// anything else is refused, never ignored.
    #[test]
    fn an_exposure_upstream_takes_only_its_own_fields() {
        for refused in [
            json!({"kind": "archi", "port": 3000}),
            json!({"kind": "hive", "hive": "worker1", "port": 8080, "address": "10.0.0.5"}),
            json!({"kind": "address", "address": "erp.example.com", "port": 8443}),
            json!({"kind": "address", "address": "10.0.0.5"}),
            json!({"kind": "hive", "hive": "worker1", "port": 70000}),
            json!({"kind": "Hive", "hive": "worker1", "port": 8080}),
            json!({"kind": "url", "url": "http://10.0.0.5/"}),
            json!({"hive": "worker1", "port": 8080}),
        ] {
            assert!(
                serde_json::from_value::<WebExposureUpstream>(refused.clone()).is_err(),
                "{refused} must be refused"
            );
        }
        assert_eq!(
            serde_json::to_value(WebExposureUpstream::Archi {}).expect("encode"),
            json!({"kind": "archi"})
        );
    }

    /// io.web's answer to a push, taken and refused, reads and writes as §5.5 gives it.
    #[test]
    fn io_web_exposures_answers_round_trip_in_the_designs_shape() {
        let taken = json!({
            "status": "ok", "generation": 1_763_121_600_123u64, "changed": true,
            "enablements": [
                {"enablement_id": "enb:0a1b2c3d-4e5f-4a6b-8c7d-8e9fa0b1c2d3", "key": "ready",
                 "detail": null},
                {"enablement_id": "enb:1b2c3d4e-5f6a-4b7c-9d8e-9fa0b1c2d3e4", "key": "unavailable",
                 "detail": "the key is at version 2, not 3"}
            ]
        });
        let answer: IoWebExposuresResponse = serde_json::from_value(taken.clone()).expect("parse");
        let IoWebExposuresResponse::Ok {
            generation,
            changed,
            enablements,
        } = &answer
        else {
            panic!("an ok answer: {answer:?}");
        };
        assert_eq!((*generation, *changed), (1_763_121_600_123, true));
        assert_eq!(enablements[0].key, IoWebKeyState::Ready);
        assert_eq!(enablements[1].key, IoWebKeyState::Unavailable);
        assert_eq!(serde_json::to_value(&answer).expect("encode"), taken);
        let (msg, got) = through_the_wire(MSG_IO_WEB_EXPOSURES_RESPONSE, &answer);
        assert_eq!((msg.as_str(), got), (MSG_IO_WEB_EXPOSURES_RESPONSE, answer));

        let refused = json!({
            "status": "error", "error_code": "STALE_GENERATION",
            "error_detail": "io.web holds generation 1763121600124"
        });
        let answer: IoWebExposuresResponse =
            serde_json::from_value(refused.clone()).expect("parse");
        assert!(matches!(
            &answer,
            IoWebExposuresResponse::Error { error_code, .. }
                if error_code == IO_WEB_ERROR_STALE_GENERATION
        ));
        assert_eq!(serde_json::to_value(&answer).expect("encode"), refused);
        // An ok answer without its generation is no answer.
        assert!(serde_json::from_value::<IoWebExposuresResponse>(
            json!({"status": "ok", "changed": false, "enablements": []})
        )
        .is_err());
    }

    /// `IO_WEB_ACCESS_LINK` and its answers read and write as §5.1 and §5.5 give them, and the
    /// answer's `Debug` never shows the link, which holds the one-time code.
    #[test]
    fn io_web_access_link_round_trips_and_never_debugs_its_code() {
        let ask = json!({
            "exposure_id": "exp:6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
            "enablement_id": "enb:0a1b2c3d-4e5f-4a6b-8c7d-8e9fa0b1c2d3",
            "generation": 1_763_121_600_123u64
        });
        let request: IoWebAccessLinkRequest = serde_json::from_value(ask.clone()).expect("parse");
        assert_eq!(serde_json::to_value(&request).expect("encode"), ask);
        let (msg, got) = through_the_wire(MSG_IO_WEB_ACCESS_LINK, &request);
        assert_eq!((msg.as_str(), got), (MSG_IO_WEB_ACCESS_LINK, request));

        let code = "L1.6f1c2a3b4d5e4f608a7b9c0d1e2f3a4b.0a1b2c3d4e5f4a6b8c7d8e9fa0b1c2d3.\
                    00112233445566778899aabbccddeeff.1763121700.deadbeef";
        let linked = json!({
            "status": "ok",
            "url": format!("https://shop.hive-k3m9x7q2.fluxbee.ai/.fluxbee/enter?code={code}"),
            "expires_at": 1_763_121_820u64,
            "link_id": "0123456789abcdef"
        });
        let answer: IoWebAccessLinkResponse =
            serde_json::from_value(linked.clone()).expect("parse");
        assert_eq!(serde_json::to_value(&answer).expect("encode"), linked);
        let (msg, got) = through_the_wire(MSG_IO_WEB_ACCESS_LINK_RESPONSE, &answer);
        assert_eq!(
            (msg.as_str(), &got),
            (MSG_IO_WEB_ACCESS_LINK_RESPONSE, &answer)
        );
        let debugged = format!("{answer:?}");
        assert!(!debugged.contains("code="), "{debugged}");
        assert!(!debugged.contains("deadbeef"), "{debugged}");
        assert!(debugged.contains("0123456789abcdef"), "{debugged}");

        let refused = json!({
            "status": "error", "error_code": "ENABLEMENT_UNKNOWN",
            "error_detail": "enb:0a1b2c3d-4e5f-4a6b-8c7d-8e9fa0b1c2d3 is not the current enablement"
        });
        let answer: IoWebAccessLinkResponse =
            serde_json::from_value(refused.clone()).expect("parse");
        assert!(matches!(
            &answer,
            IoWebAccessLinkResponse::Error { error_code, .. }
                if error_code == IO_WEB_ERROR_ENABLEMENT_UNKNOWN
        ));
        assert_eq!(serde_json::to_value(&answer).expect("encode"), refused);
        assert!(format!("{answer:?}").contains("ENABLEMENT_UNKNOWN"));
    }
}
