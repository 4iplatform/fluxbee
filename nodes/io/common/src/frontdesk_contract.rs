use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const FRONTDESK_HANDOFF_PAYLOAD_TYPE: &str = "frontdesk_handoff";
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

pub fn parse_frontdesk_result_payload(payload: &Value) -> Option<FrontdeskResultPayload> {
    let payload_type = payload.get("type")?.as_str()?;
    if payload_type != FRONTDESK_RESULT_PAYLOAD_TYPE {
        return None;
    }
    serde_json::from_value(payload.clone()).ok()
}

pub fn frontdesk_result_human_message(payload: &Value) -> Option<&str> {
    let parsed = parse_frontdesk_result_payload(payload)?;
    if parsed.human_message.trim().is_empty() {
        return None;
    }
    payload.get("human_message")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_frontdesk_result_payload() {
        let payload = json!({
            "type": "frontdesk_result",
            "schema_version": 1,
            "status": "needs_input",
            "result_code": "MISSING_REQUIRED_FIELDS",
            "human_message": "Necesito tu email para continuar.",
            "missing_fields": ["email"],
            "error_code": null,
            "error_detail": null,
            "ilk_id": "ilk:123",
            "tenant_id": "tnt:acme",
            "registration_status": "temporary"
        });

        let parsed = parse_frontdesk_result_payload(&payload).expect("parsed");
        assert_eq!(parsed.payload_type, FRONTDESK_RESULT_PAYLOAD_TYPE);
        assert_eq!(parsed.schema_version, FRONTDESK_SCHEMA_VERSION_V1);
        assert_eq!(parsed.result_code, "MISSING_REQUIRED_FIELDS");
        assert_eq!(
            frontdesk_result_human_message(&payload),
            Some("Necesito tu email para continuar.")
        );
    }

    #[test]
    fn ignores_non_frontdesk_result_payload() {
        let payload = json!({
            "type": "text",
            "content": "hola"
        });
        assert!(parse_frontdesk_result_payload(&payload).is_none());
        assert!(frontdesk_result_human_message(&payload).is_none());
    }

    #[test]
    fn parses_frontdesk_handoff_payload() {
        let payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": {
                "display_name": "Juan Perez",
                "email": "juan@example.com",
                "company_name": "ACME",
                "attributes": {
                    "crm_customer_id": "crm-123"
                }
            },
            "tenant_id": "tnt:acme",
            "context": {
                "source_node": "IO.api.frontdesk@motherbee"
            }
        });

        let parsed = parse_frontdesk_handoff_payload(&payload).expect("parsed");
        assert_eq!(parsed.payload_type, FRONTDESK_HANDOFF_PAYLOAD_TYPE);
        assert_eq!(parsed.operation, "complete_registration");
        assert_eq!(parsed.subject.display_name.as_deref(), Some("Juan Perez"));
        assert_eq!(parsed.subject.email.as_deref(), Some("juan@example.com"));
        assert_eq!(parsed.subject.company_name.as_deref(), Some("ACME"));
        assert_eq!(parsed.tenant_id.as_deref(), Some("tnt:acme"));
    }
}
