//! The packages SY.deploy's tests deploy: the plain ED as the .deb ships it, its copy as SY.deploy
//! kept it before stage 3, and a capability with a WF at its door, an agent and a connector (of one
//! ED, and of every ED of its tenant).

use serde_json::{json, Value};

use json_router::solution_package::{parse_package_value, ParsedPackage, Reading};

/// The plain ED, as the .deb ships it.
pub const PLAIN_ED: &str = include_str!("../../../packaging/solutions/plain-ed/1.json");

pub fn plain_ed_value() -> Value {
    serde_json::from_str(PLAIN_ED).expect("the plain ED is JSON")
}

pub fn plain_ed() -> ParsedPackage {
    parse_package_value(&plain_ed_value(), Reading::Publish).expect("the plain ED parses")
}

/// The plain ED's copy as SY.deploy kept it before stage 3, on 8.x's disk: kind `ed`, no offer.
pub fn plain_ed_kept_before_stage_3() -> Value {
    let mut value = plain_ed_value();
    value["kind"] = json!("ed");
    let fields = value.as_object_mut().expect("an object");
    fields.remove("origin");
    fields.remove("offered_to");
    value
}

pub fn plain_blanks() -> Value {
    json!({
        "display_name": "Laura Gómez",
        "company_name": "Acme",
        "personality": {
            "timezone": "America/Argentina/Buenos_Aires",
            "country_code": "AR",
            "primary_language": "es",
            "additional_languages": ["en"],
            "description": "Warm and direct.",
            "biographical": {"born": "Rosario"},
            "narrative": {"voice": "calm"}
        },
        "whatsapp": {
            "phone_number_id": "1234567890",
            "waba_id": "998877",
            "credential_key": "acme-laura-wapp"
        }
    })
}

/// The billing capability, as JSON.
const BILLING: &str = r#"{
  "schema": 1,
  "id": "billing",
  "version": "1",
  "kind": "capability",
  "origin": "catalog",
  "offered_to": {
    "tenants": "all",
    "eds": "all"
  },
  "title": "Billing",
  "description": "Invoices: the state of an invoice, by its number.",
  "blanks": [
    {
      "name": "crm_key",
      "type": "vault_key",
      "resource_type": "api_key",
      "description": "The CRM's key, by its name in the tenant's vault."
    }
  ],
  "nodes": [
    {
      "role": "billing",
      "kind": "AI",
      "middle": "billing",
      "runtime": "ai.generic",
      "work_role": "agent",
      "provider": "anthropic",
      "config": {
        "behavior": {
          "kind": "ai_chat",
          "model": "claude-sonnet-5-5"
        },
        "crm": {
          "$ref": "nodes.crm.node_name"
        }
      },
      "definition": {
        "role": {
          "asset_type": "role",
          "name": "Billing",
          "description": "You read the state of invoices in the CRM and answer with it."
        }
      }
    },
    {
      "role": "crm",
      "kind": "IO",
      "middle": "crm",
      "runtime": "io.api",
      "work_role": "connector",
      "config": {
        "auth": {
          "type": "vault_ref",
          "resource_type": "api_key",
          "key": {
            "$blank": "crm_key"
          }
        },
        "dst_node": {
          "$ref": "nodes.billing.node_name"
        }
      }
    },
    {
      "role": "door",
      "kind": "WF",
      "middle": "billing",
      "work_role": "capability",
      "definition": {
        "wf_schema_version": "1",
        "workflow_type": "capability.billing",
        "description": "Invoices: the state of an invoice, by its number.",
        "start_from": [
          {
            "$ref": "grouping.conscious"
          }
        ],
        "request_actions": [
          {
            "name": "invoice_state",
            "description": "The state of an invoice, by its number."
          }
        ],
        "input_schema": {
          "type": "object",
          "required": [
            "number"
          ],
          "properties": {
            "number": {
              "type": "string",
              "minLength": 1
            }
          }
        },
        "initial_state": "reading",
        "terminal_states": [
          "answered"
        ],
        "states": [
          {
            "name": "reading",
            "entry_actions": [
              {
                "type": "send_message",
                "target": {
                  "$ref": "nodes.billing.node_name"
                },
                "meta": {
                  "msg": "INVOICE_STATE"
                },
                "payload": {
                  "type": "text",
                  "content": "Read this invoice's state in the CRM."
                },
                "vars": {
                  "number": {
                    "$ref": "input.number"
                  }
                }
              }
            ],
            "transitions": [
              {
                "event_match": {
                  "src": {
                    "$ref": "nodes.billing.node_name"
                  }
                },
                "guard": "true",
                "target_state": "answered",
                "actions": [
                  {
                    "type": "reply",
                    "status": "done",
                    "result": {
                      "state": {
                        "$ref": "event.context.evaluation.params.state",
                        "default": ""
                      }
                    }
                  }
                ]
              }
            ]
          },
          {
            "name": "answered"
          }
        ]
      }
    }
  ],
  "vault": [
    {
      "key": {
        "$blank": "crm_key"
      },
      "resource_type": "api_key",
      "to": "crm"
    }
  ]
}"#;

/// A capability of one ED: its agent reads invoices in a CRM through its connector, and a WF at
/// its door, started by the ED's conscious, asks the agent and replies.
pub fn billing_value() -> Value {
    serde_json::from_str(BILLING).expect("the billing fixture is JSON")
}

pub fn billing() -> ParsedPackage {
    parse_package_value(&billing_value(), Reading::Publish).expect("billing parses")
}

pub fn billing_blanks() -> Value {
    json!({"crm_key": "acme-crm"})
}

/// The billing capability for every ED of its tenant: its door starts from the consciouses.
pub fn every_ed_value() -> Value {
    let mut value = billing_value();
    value["id"] = json!("billing-all");
    value["nodes"][2]["definition"]["start_from"] = json!([{"consciouses": "tenant"}]);
    value
}

pub fn every_ed() -> ParsedPackage {
    parse_package_value(&every_ed_value(), Reading::Publish).expect("billing-all parses")
}
