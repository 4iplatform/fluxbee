//! Cloud's tree (docs/digital-executive-v1-stage-2.md §3.4, stage 3 §7), read when it is asked: a
//! tenant's general infra (its tenant grouping's deployments: its infrastructure and its
//! capabilities of every ED), its EDs (the tenant's virtual persons in the identity SHM, each with
//! its document), each ED's parts (the ILKs whose `works_for` names it: its conscious, its
//! channels, its capabilities' doors, agents and connectors) and its deployments (SY.deploy's
//! records of its grouping, the executive's and each capability's). Key names only, never a
//! secret.

use serde_json::{json, Value};

use crate::store::DeploymentRecord;
use crate::world::{DocumentRead, IlkView};

fn deployment_leaf(record: &DeploymentRecord) -> Value {
    json!({
        "deployment_id": record.deployment_id,
        "source": record.source,
        "state": record.state.as_str(),
        "hive": record.hive,
        "nodes": record.nodes.iter().map(|node| json!({
            "role": node.role,
            "node_name": node.node_name,
            "ilk_id": node.ilk_id,
            "work_role": node.work_role,
            "runtime": node.runtime,
            "runtime_version": node.runtime_version,
        })).collect::<Vec<_>>(),
        "assets": record.assets,
        "credentials": record.vault.iter().map(|entry| json!({
            "key": entry.key,
            "resource_type": entry.resource_type,
            "assigned_to": entry.assigned_to,
        })).collect::<Vec<_>>(),
        "created_at_ms": record.created_at_ms,
        "updated_at_ms": record.updated_at_ms,
    })
}

/// The tree of `tenant_id`, from the SHM's ILKs, the records, and the people folder
/// (`document`, by the person's ILK and its document's 16-hex prefix). The nodes' status is the
/// caller's to add.
pub fn assemble_tree(
    tenant_id: &str,
    ilks: &[IlkView],
    records: &[DeploymentRecord],
    document: impl Fn(&str, &str) -> DocumentRead,
) -> Value {
    let mut records: Vec<&DeploymentRecord> = records
        .iter()
        .filter(|record| record.tenant_id == tenant_id)
        .collect();
    records.sort_by(|a, b| {
        a.created_at_ms
            .cmp(&b.created_at_ms)
            .then_with(|| a.deployment_id.cmp(&b.deployment_id))
    });
    let infra: Vec<Value> = records
        .iter()
        .filter(|record| record.grouping.kind == json_router::solution_package::GROUPING_TENANT)
        .map(|record| deployment_leaf(record))
        .collect();
    let mut persons: Vec<&IlkView> = ilks
        .iter()
        .filter(|ilk| ilk.tenant_id == tenant_id && ilk.ilk_type == "human" && ilk.is_virtual)
        .collect();
    persons.sort_by(|a, b| a.ilk_id.cmp(&b.ilk_id));
    let eds: Vec<Value> = persons
        .into_iter()
        .map(|person| {
            let (document_state, document_value) = match &person.document_hash {
                None => ("none", Value::Null),
                Some(hash16) => match document(&person.ilk_id, hash16) {
                    DocumentRead::Found(value) => ("ok", value),
                    DocumentRead::NotYet => ("not_yet", Value::Null),
                    DocumentRead::Unreadable(why) => {
                        tracing::warn!(ilk_id = %person.ilk_id, error = %why, "a person's document does not read");
                        ("unreadable", Value::Null)
                    }
                },
            };
            let mut parts: Vec<&IlkView> = ilks
                .iter()
                .filter(|ilk| ilk.works_for.as_deref() == Some(person.ilk_id.as_str()))
                .collect();
            parts.sort_by(|a, b| a.ilk_id.cmp(&b.ilk_id));
            let deployments: Vec<Value> = records
                .iter()
                .filter(|record| record.grouping.id == person.ilk_id)
                .map(|record| deployment_leaf(record))
                .collect();
            let short_id = records
                .iter()
                .find(|record| record.grouping.id == person.ilk_id)
                .map(|record| record.grouping.short_id.clone());
            json!({
                "ilk_id": person.ilk_id,
                "display_name": person.display_name,
                "short_id": short_id,
                "document_state": document_state,
                "document": document_value,
                "parts": parts.iter().map(|part| json!({
                    "ilk_id": part.ilk_id,
                    "work_role": part.work_role,
                    "node_name": part.handler_node,
                })).collect::<Vec<_>>(),
                "deployments": deployments,
            })
        })
        .collect();
    json!({
        "tenant_id": tenant_id,
        "infra": infra,
        "eds": eds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::tests::{plain_record, CHANNEL, CONSCIOUS, PERSON};
    use crate::world::fake::{OTHER_TENANT, TENANT};

    #[test]
    fn the_tree_holds_the_tenant_s_eds_their_parts_and_deployments_with_no_secret() {
        let mut record = plain_record();
        record.vault = vec![crate::store::RecordVault {
            key: "acme-laura-wapp".to_string(),
            resource_type: "whatsapp".to_string(),
            assigned_to: "IO.wapp.ed4qm7k2xa@worker1".to_string(),
        }];
        let mut other_tenant = plain_record();
        other_tenant.deployment_id = "dp2zzzzzzz".to_string();
        other_tenant.tenant_id = OTHER_TENANT.to_string();
        let ilks = vec![
            IlkView {
                ilk_id: PERSON.to_string(),
                ilk_type: "human".to_string(),
                tenant_id: TENANT.to_string(),
                display_name: Some("Laura Gómez".to_string()),
                is_virtual: true,
                document_hash: Some("0123456789abcdef".to_string()),
                ..IlkView::default()
            },
            IlkView {
                ilk_id: CONSCIOUS.to_string(),
                ilk_type: "agent".to_string(),
                tenant_id: TENANT.to_string(),
                handler_node: Some("AI.ed4qm7k2xa@worker1".to_string()),
                works_for: Some(PERSON.to_string()),
                work_role: Some("conscious".to_string()),
                ..IlkView::default()
            },
            IlkView {
                ilk_id: CHANNEL.to_string(),
                ilk_type: "agent".to_string(),
                tenant_id: TENANT.to_string(),
                handler_node: Some("IO.wapp.ed4qm7k2xa@worker1".to_string()),
                works_for: Some(PERSON.to_string()),
                work_role: Some("channel".to_string()),
                ..IlkView::default()
            },
            // A real person, and another tenant's ED: neither is in the tree.
            IlkView {
                ilk_id: "ilk:550e8400-e29b-41d4-a716-446655440009".to_string(),
                ilk_type: "human".to_string(),
                tenant_id: TENANT.to_string(),
                ..IlkView::default()
            },
            IlkView {
                ilk_id: "ilk:550e8400-e29b-41d4-a716-446655440010".to_string(),
                ilk_type: "human".to_string(),
                tenant_id: OTHER_TENANT.to_string(),
                is_virtual: true,
                ..IlkView::default()
            },
        ];
        let tree = assemble_tree(TENANT, &ilks, &[record, other_tenant], |ilk_id, hash16| {
            assert_eq!((ilk_id, hash16), (PERSON, "0123456789abcdef"));
            DocumentRead::Found(json!({"schema": 1, "display_name": "Laura Gómez"}))
        });
        assert_eq!(tree["tenant_id"], json!(TENANT));
        assert_eq!(tree["infra"], json!([]));
        let eds = tree["eds"].as_array().expect("eds");
        assert_eq!(eds.len(), 1, "{tree}");
        let ed = &eds[0];
        assert_eq!(ed["ilk_id"], json!(PERSON));
        assert_eq!(ed["short_id"], json!("ed4qm7k2xa"));
        assert_eq!(ed["document_state"], json!("ok"));
        assert_eq!(ed["document"]["display_name"], json!("Laura Gómez"));
        let parts: Vec<(&str, &str)> = ed["parts"]
            .as_array()
            .expect("parts")
            .iter()
            .map(|part| {
                (
                    part["work_role"].as_str().unwrap_or(""),
                    part["node_name"].as_str().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            parts,
            [
                ("conscious", "AI.ed4qm7k2xa@worker1"),
                ("channel", "IO.wapp.ed4qm7k2xa@worker1")
            ]
        );
        let deployments = ed["deployments"].as_array().expect("deployments");
        assert_eq!(deployments.len(), 1);
        assert_eq!(deployments[0]["deployment_id"], json!("dp7xk3m2qa"));
        assert_eq!(
            deployments[0]["credentials"][0]["key"],
            json!("acme-laura-wapp")
        );
        // Key names only: no blank, no config, nothing that could hold a secret.
        assert!(deployments[0].get("blanks").is_none());
        let text = tree.to_string();
        assert!(
            !text.contains("998877") && !text.contains("access_token"),
            "{text}"
        );
    }

    #[test]
    fn a_document_that_has_not_arrived_reads_not_yet() {
        let ilks = vec![IlkView {
            ilk_id: PERSON.to_string(),
            ilk_type: "human".to_string(),
            tenant_id: TENANT.to_string(),
            is_virtual: true,
            document_hash: Some("0123456789abcdef".to_string()),
            ..IlkView::default()
        }];
        let tree = assemble_tree(TENANT, &ilks, &[], |_, _| DocumentRead::NotYet);
        assert_eq!(tree["eds"][0]["document_state"], json!("not_yet"));
        assert_eq!(tree["eds"][0]["document"], Value::Null);
    }
}
