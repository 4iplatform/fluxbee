//! First-contact gate: hand an unregistered sender to AI.frontdesk before relaying.
//!
//! # Why it exists
//!
//! When someone writes for the first time from an external channel, the IO node creates an ILK for
//! them with `registration_status = "temporary"` and `identification: {}`: an empty handle for
//! "someone who wrote and we don't know yet who they are". The spec
//! (`docs/history/10-identity-v2.md`) says that ILK is promoted by **AI.frontdesk**, the only one
//! besides SY.orchestrator allowed to emit `ILK_REGISTER`.
//!
//! But nothing took the stranger there, so the ILK stayed `temporary` forever.
//!
//! # What it does, and what it does NOT do
//!
//! It follows io.api's pattern (`requires_frontdesk_intermediate` in `io-api/src/main.rs`):
//! **gate-then-forward**. The frontdesk gets a STRUCTURED payload (the deterministic path, the same
//! one already running in production), and if that goes well the original message **travels on to
//! its real destination**. The user's message is not lost, and the user is not asked to rewrite it.
//!
//! It uses the frontdesk's STRUCTURED (deterministic) path on purpose, not the conversational LLM
//! one. The conversational path is NOT dead (it is reachable when the router's force rule,
//! temporary/None->frontdesk since 0.1.20, diverts a first-contact human with their plain
//! message); for THIS gate the structured path is the right one because the data already comes
//! structured. The conversational path's historic bug (it answered `REGISTERED`/`complete` without
//! having called `ILK_REGISTER`) was fixed in 2026-08 (success persists `status=completed`; `None`
//! now means "no registration this turn" -> `needs_input`).
//!
//! # Off by default
//!
//! `FrontdeskGateConfig::default()` comes with `enabled: false`, and the node behaves exactly as
//! before. The machinery stays installed and is switched on by CONFIG_SET once the frontdesk side
//! is settled.
//!
//! # What this module does NOT solve, on purpose
//!
//! The router ALREADY has the equivalent detour (`apply_identity_pre_resolve`: if the sender is
//! `temporary`, it forces the destination to the frontdesk) and it cuts in before OPA, with no Rego
//! policy needed. But io.slack/io.wapp cannot reach it: with `dst_node` configured they emit
//! `Destination::Unicast` and never go through the pre-resolve. Moving to `Destination::Resolve`
//! would be the intended design, but today NO user OPA policy is loaded: any message the
//! pre-resolve does not catch would fall into `OpaError::NotLoaded` and die. That is why the gate
//! lives in the node, which is deterministic and does not depend on OPA.

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use serde_json::{json, Value};

use crate::frontdesk_contract::{FrontdeskHandoffPayload, FrontdeskHandoffSubject};

/// The registrar's default name. It is the same one io.api uses and the one the router reads from
/// `government.identity_frontdesk`.
pub const DEFAULT_FRONTDESK_TARGET: &str = "SY.frontdesk.gov@motherbee";

/// The type and the operation of the structured payload, as the frontdesk expects them.
pub const FRONTDESK_HANDOFF_TYPE: &str = "frontdesk_handoff";
pub const FRONTDESK_OPERATION_REGISTER: &str = "complete_registration";

/// The reply contract a producer (io.api / io.cloud) puts in the message's `response_envelope` so
/// the frontdesk emits its STRUCTURED verdict (`{success, human_message, error_code?}`) instead of
/// plain text (`build_frontdesk_result_reply`). One source shared by the producers; it used to live
/// privately in io.api.
pub fn frontdesk_response_contract() -> serde_json::Value {
    serde_json::json!({
        "kind": "json_object_v1",
        "required": ["success", "human_message"],
        "properties": {
            "success": {"type": "boolean"},
            "human_message": {"type": "string"},
            "error_code": {"type": "string"},
            "ilk_id": {"type": "string"},
            "merged": {"type": "boolean"}
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontdeskGateConfig {
    /// OFF by default. Switching it on changes the behavior of a live hive.
    pub enabled: bool,
    pub target: String,
}

impl Default for FrontdeskGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target: DEFAULT_FRONTDESK_TARGET.to_string(),
        }
    }
}

impl FrontdeskGateConfig {
    /// Reads `io.frontdesk.{enabled,target}` from the node's effective config.
    ///
    /// Nothing at all => off. An empty or blank `target` switches nothing on: it falls back to the
    /// default, because a gate switched on and pointed at "" would send the first contact nowhere.
    pub fn from_effective_config(effective: Option<&Value>) -> Self {
        let node = effective
            .and_then(|c| c.get("io"))
            .and_then(|io| io.get("frontdesk"));
        let enabled = node
            .and_then(|f| f.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let target = node
            .and_then(|f| f.get("target"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_FRONTDESK_TARGET)
            .to_string();
        Self { enabled, target }
    }
}

/// True when the sender is not registered yet and has to go through the frontdesk.
///
/// `None` (we could not read the status) does NOT open the gate: not knowing, the message is
/// delivered as always. The alternative (holding everything we cannot classify) turns a failed SHM
/// read into a channel outage.
pub fn gate_required(registration_status: Option<&str>) -> bool {
    registration_status
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(|status| !status.eq_ignore_ascii_case("complete"))
}

/// Reads the sender's `registration_status` from the identity SHM.
///
/// The same route io.api uses (`resolve_identity_option_from_hive_id`). Returns `None` when it
/// cannot be resolved, and by `gate_required` that means "deliver as usual".
pub fn sender_registration_status(
    hive_id: &str,
    channel_type: &str,
    address: &str,
    tenant_id: &str,
) -> Option<String> {
    if hive_id.is_empty() {
        return None;
    }
    match fluxbee_sdk::resolve_identity_option_from_hive_id(
        hive_id,
        channel_type,
        address,
        tenant_id,
    ) {
        Ok(Some(option)) => Some(option.ilk.registration_status),
        Ok(None) => None,
        Err(err) => {
            tracing::debug!(
                channel_type = %channel_type,
                error = %err,
                "could not read the sender's registration_status; delivering without the frontdesk"
            );
            None
        }
    }
}

/// Builds the structured handoff message for the frontdesk.
///
/// Every field of `FrontdeskHandoffSubject` is optional, so a channel that only knows an external
/// user id can still introduce the subject: it goes in `attributes`, and the frontdesk knows it has
/// to find out the rest.
pub fn build_handoff_message(
    node_uuid: &str,
    cfg: &FrontdeskGateConfig,
    src_ilk: &str,
    tenant_id: Option<&str>,
    channel_type: &str,
    address: &str,
    trace_id: &str,
) -> Message {
    let mut attributes = serde_json::Map::new();
    attributes.insert("channel_type".to_string(), json!(channel_type));
    attributes.insert("address".to_string(), json!(address));

    let payload = FrontdeskHandoffPayload {
        payload_type: FRONTDESK_HANDOFF_TYPE.to_string(),
        schema_version: 1,
        operation: FRONTDESK_OPERATION_REGISTER.to_string(),
        subject: FrontdeskHandoffSubject {
            display_name: None,
            email: None,
            phone: None,
            company_name: None,
            attributes: Some(attributes),
        },
        tenant_id: tenant_id.map(ToString::to_string),
        context: None,
    };

    Message {
        routing: Routing {
            src: node_uuid.to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(cfg.target.clone()),
            ttl: 16,
            trace_id: trace_id.to_string(),
        },
        meta: Meta {
            // A USER message, like io.cloud's handoff: the frontdesk only takes a handoff from
            // `user` messages, so a `data` one never reached its deterministic path.
            msg_type: "user".to_string(),
            msg: Some(crate::frontdesk_contract::FRONTDESK_HANDOFF_MSG.to_string()),
            src_ilk: Some(src_ilk.to_string()),
            ..Meta::default()
        },
        payload: serde_json::to_value(payload).unwrap_or_else(|_| json!({})),
    }
}

/// Takes the sender through the frontdesk when needed, then lets the original message go on.
///
/// It always returns `()`: it is a GATE THAT DOES NOT HOLD. If the handoff fails (frontdesk down,
/// no route), it is logged at `error` and the user's message **is delivered anyway**.
///
/// That choice is deliberate and it is the only one in this module that is not purely technical:
/// dropping a customer's message because an internal node did not answer is worse than delivering
/// it unregistered. The price is that the registration can be lost silently, except for that log.
/// If the opposite is preferred (fail-closed: no delivery to strangers), it is a one-line change
/// here, but it is the operator's decision, not the code's.
pub async fn gate_then_forward(
    sender: &fluxbee_sdk::NodeSender,
    cfg: &FrontdeskGateConfig,
    hive_id: &str,
    channel_type: &str,
    address: &str,
    tenant_id: Option<&str>,
    src_ilk: Option<&str>,
    trace_id: &str,
) {
    if !cfg.enabled {
        return;
    }
    let Some(src_ilk) = src_ilk.map(str::trim).filter(|v| !v.is_empty()) else {
        // Without an ILK there is no subject to introduce. It is not an error: it is a sender that
        // could not even be provisioned, and the identity path already logs that.
        return;
    };
    let status =
        sender_registration_status(hive_id, channel_type, address, tenant_id.unwrap_or(""));
    if !gate_required(status.as_deref()) {
        return;
    }

    let msg = build_handoff_message(
        sender.uuid(),
        cfg,
        src_ilk,
        tenant_id,
        channel_type,
        address,
        trace_id,
    );
    tracing::info!(
        target_node = %cfg.target,
        src_ilk = %src_ilk,
        registration_status = %status.as_deref().unwrap_or("?"),
        %trace_id,
        "first contact: introducing the sender to the frontdesk before delivering"
    );
    if let Err(err) = sender.send(msg).await {
        tracing::error!(
            error = ?err,
            target_node = %cfg.target,
            src_ilk = %src_ilk,
            %trace_id,
            "could not introduce the sender to the frontdesk; the message is delivered anyway and \
             the registration stays pending"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_compuerta_viene_apagada() {
        let cfg = FrontdeskGateConfig::default();
        assert!(
            !cfg.enabled,
            "switching it on changes a live hive: default = off"
        );
        assert_eq!(cfg.target, DEFAULT_FRONTDESK_TARGET);

        // A config without the section => off.
        let vacia = FrontdeskGateConfig::from_effective_config(Some(&json!({"io":{}})));
        assert!(!vacia.enabled);
        // And with no config at all, too.
        assert!(!FrontdeskGateConfig::from_effective_config(None).enabled);
    }

    #[test]
    fn se_enciende_solo_por_config_explicita() {
        let cfg = FrontdeskGateConfig::from_effective_config(Some(&json!({
            "io": {"frontdesk": {"enabled": true, "target": "SY.frontdesk.gov@motherbee"}}
        })));
        assert!(cfg.enabled);
        assert_eq!(cfg.target, "SY.frontdesk.gov@motherbee");
    }

    #[test]
    fn un_target_vacio_no_manda_el_primer_contacto_a_la_nada() {
        let cfg = FrontdeskGateConfig::from_effective_config(Some(&json!({
            "io": {"frontdesk": {"enabled": true, "target": "   "}}
        })));
        assert!(cfg.enabled);
        assert_eq!(
            cfg.target, DEFAULT_FRONTDESK_TARGET,
            "a blank target falls back to the default, not to an empty string"
        );
    }

    #[test]
    fn solo_pasa_por_el_frontdesk_quien_no_esta_completo() {
        assert!(gate_required(Some("temporary")));
        assert!(gate_required(Some("partial")));
        assert!(!gate_required(Some("complete")));
        assert!(
            !gate_required(Some("COMPLETE")),
            "the status is case-insensitive"
        );

        // Not knowing does NOT hold the channel.
        assert!(!gate_required(None));
        assert!(!gate_required(Some("")));
        assert!(!gate_required(Some("   ")));
    }

    #[test]
    fn el_handoff_presenta_al_sujeto_aunque_solo_haya_un_user_id() {
        let cfg = FrontdeskGateConfig::default();
        let msg = build_handoff_message(
            "uuid-nodo",
            &cfg,
            "ilk:abc",
            Some("tnt:1111"),
            "slack",
            "IO.slack.test@motherbee:U0HUMANO",
            "trace-1",
        );

        match &msg.routing.dst {
            Destination::Unicast(dst) => assert_eq!(dst, &cfg.target),
            other => panic!("the handoff goes unicast to the frontdesk, not {other:?}"),
        }
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:abc"));
        assert_eq!(
            msg.meta.msg.as_deref(),
            Some(crate::frontdesk_contract::FRONTDESK_HANDOFF_MSG)
        );
        // The frontdesk only takes handoffs from `user` messages (like io.cloud's).
        assert_eq!(msg.meta.msg_type, "user");

        // It is the STRUCTURED path, not the conversational one: it has to parse as a handoff.
        let parsed = crate::frontdesk_contract::parse_frontdesk_handoff_payload(&msg.payload)
            .expect("the frontdesk has to be able to parse it as a structured handoff");
        assert_eq!(parsed.operation, FRONTDESK_OPERATION_REGISTER);
        assert_eq!(parsed.tenant_id.as_deref(), Some("tnt:1111"));
        let attrs = parsed.subject.attributes.expect("attributes");
        assert_eq!(
            attrs.get("channel_type").and_then(Value::as_str),
            Some("slack")
        );
        assert!(attrs
            .get("address")
            .and_then(Value::as_str)
            .is_some_and(|a| a.contains("U0HUMANO")));
    }
}
