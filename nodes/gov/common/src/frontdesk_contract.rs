use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const FRONTDESK_HANDOFF_PAYLOAD_TYPE: &str = "frontdesk_handoff";
/// The `meta.msg` its producer stamps on a handoff message.
pub const FRONTDESK_HANDOFF_MSG: &str = "frontdesk_handoff";
pub const FRONTDESK_RESULT_PAYLOAD_TYPE: &str = "frontdesk_result";
pub const FRONTDESK_SCHEMA_VERSION_V1: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontdeskHandoffPayload {
    #[serde(rename = "type")]
    pub payload_type: String,
    pub schema_version: u32,
    pub operation: String,
    #[serde(default)]
    pub subject: FrontdeskHandoffSubject,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub context: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FrontdeskHandoffSubject {
    #[serde(default)]
    pub display_name: Option<String>,
    /// Vouched: the IO node that sends the handoff answers for it (the integrator holding io.api's
    /// bearer, the login Fluxbee Cloud authenticated). The frontdesk registers it as the person's
    /// `identification.email`, where an email already registered in the tenant merges.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub attributes: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontdeskResultPayload {
    #[serde(rename = "type")]
    pub payload_type: String,
    pub schema_version: u32,
    pub status: String,
    pub result_code: String,
    pub human_message: String,
    #[serde(default)]
    pub missing_fields: Vec<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub error_detail: Option<String>,
    #[serde(default)]
    pub ilk_id: Option<String>,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub registration_status: Option<String>,
}

pub fn parse_frontdesk_handoff_payload(payload: &Value) -> Option<FrontdeskHandoffPayload> {
    let payload_type = payload.get("type")?.as_str()?;
    if payload_type != FRONTDESK_HANDOFF_PAYLOAD_TYPE {
        return None;
    }
    serde_json::from_value(payload.clone()).ok()
}

/// A handoff is a message its producer marks as one (`meta.msg` is `frontdesk_handoff`) whose
/// payload parses as one. The payload alone never makes a handoff: an IO node may relay a payload
/// someone else wrote.
pub fn parse_frontdesk_handoff(
    meta_msg: Option<&str>,
    payload: &Value,
) -> Option<FrontdeskHandoffPayload> {
    if meta_msg != Some(FRONTDESK_HANDOFF_MSG) {
        return None;
    }
    parse_frontdesk_handoff_payload(payload)
}

/// Only an IO node hands a person over, since it vouches for the subject's email
/// (docs/control-path-hardening-v1.md §7). `src_l2_name` is the sender the router stamped on the
/// message, never anything the payload says.
pub fn frontdesk_handoff_sender_allowed(src_l2_name: Option<&str>) -> bool {
    src_l2_name
        .map(str::trim)
        .is_some_and(|sender| sender.starts_with("IO."))
}

pub fn frontdesk_result_payload(
    status: impl Into<String>,
    result_code: impl Into<String>,
    human_message: impl Into<String>,
) -> FrontdeskResultPayload {
    FrontdeskResultPayload {
        payload_type: FRONTDESK_RESULT_PAYLOAD_TYPE.to_string(),
        schema_version: FRONTDESK_SCHEMA_VERSION_V1,
        status: status.into(),
        result_code: result_code.into(),
        human_message: human_message.into(),
        missing_fields: Vec::new(),
        error_code: None,
        error_detail: None,
        ilk_id: None,
        tenant_id: None,
        registration_status: None,
    }
}

impl FrontdeskResultPayload {
    pub fn to_value(&self) -> serde_json::Result<Value> {
        serde_json::to_value(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_frontdesk_handoff_payload() {
        let payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": {
                "display_name": "Juan Perez",
                "email": "juan@example.com",
                "company_name": "ACME"
            },
            "tenant_id": "tnt:acme"
        });
        let parsed = parse_frontdesk_handoff_payload(&payload).expect("parsed");
        assert_eq!(parsed.operation, "complete_registration");
        assert_eq!(parsed.subject.display_name.as_deref(), Some("Juan Perez"));
        assert_eq!(parsed.tenant_id.as_deref(), Some("tnt:acme"));
    }

    #[test]
    fn a_handoff_needs_its_producers_mark() {
        let payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": { "display_name": "Juan Perez", "email": "juan@example.com" }
        });
        assert!(parse_frontdesk_handoff(Some(FRONTDESK_HANDOFF_MSG), &payload).is_some());
        // The same payload relayed without the mark, or under another message name.
        for meta_msg in [None, Some(""), Some("text"), Some("FRONTDESK_HANDOFF")] {
            assert!(
                parse_frontdesk_handoff(meta_msg, &payload).is_none(),
                "{meta_msg:?}"
            );
        }
        // The mark without a handoff payload.
        let text = json!({ "type": "text", "content": "hola" });
        assert!(parse_frontdesk_handoff(Some(FRONTDESK_HANDOFF_MSG), &text).is_none());
    }

    #[test]
    fn only_an_io_node_sends_a_handoff() {
        for sender in [
            "IO.api@motherbee",
            "IO.cloud@motherbee",
            "IO.slack.support@worker1",
        ] {
            assert!(frontdesk_handoff_sender_allowed(Some(sender)), "{sender}");
        }
        for sender in [
            None,
            Some(""),
            Some("AI.support@motherbee"),
            Some("WF.onboarding@motherbee"),
            Some("SY.admin@motherbee"),
            Some("SY.frontdesk.gov@motherbee"),
            Some("io.api@motherbee"),
        ] {
            assert!(!frontdesk_handoff_sender_allowed(sender), "{sender:?}");
        }
    }

    #[test]
    fn builds_frontdesk_result_payload() {
        let payload =
            frontdesk_result_payload("needs_input", "MISSING_REQUIRED_FIELDS", "Falta email");
        assert_eq!(payload.payload_type, FRONTDESK_RESULT_PAYLOAD_TYPE);
        assert_eq!(payload.schema_version, FRONTDESK_SCHEMA_VERSION_V1);
        assert_eq!(payload.result_code, "MISSING_REQUIRED_FIELDS");
        assert_eq!(payload.human_message, "Falta email");
    }
}
