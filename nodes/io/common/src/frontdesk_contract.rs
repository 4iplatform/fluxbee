use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const FRONTDESK_HANDOFF_PAYLOAD_TYPE: &str = "frontdesk_handoff";
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

pub fn parse_frontdesk_handoff_payload(payload: &Value) -> Option<FrontdeskHandoffPayload> {
    let payload_type = payload.get("type")?.as_str()?;
    if payload_type != FRONTDESK_HANDOFF_PAYLOAD_TYPE {
        return None;
    }
    serde_json::from_value(payload.clone()).ok()
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
