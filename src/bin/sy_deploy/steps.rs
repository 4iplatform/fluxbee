//! The step builder (docs/digital-executive-v1-stage-2.md §3, §5.2): from the desired state and
//! what runs, the SY.admin actions still missing, in order, with their exact arguments. One builder
//! for every source: a new kind of part is one more section here.
//!
//! The plain ED's ten steps, from nothing: the ED and its document first; then each node, in the
//! source's order (the conscious before its IO), its assets, its launch (which makes its ILK), its
//! relation and its definition; then the keys; then the record. What runs already is not sent
//! again: a retry converges, it does not replay (§5.2).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{json, Value};

use crate::desired::{DesiredNode, DesiredState};
use crate::world::{AdminCall, IlkView};

/// How long SY.deploy waits for one SY.admin action: longer than SY.admin's own wait on the
/// orchestrator and on SY.identity (30 s), so it never gives up while SY.admin's call still runs.
pub const ACTION_WAIT: Duration = Duration::from_secs(40);

/// A `kill_node`'s wait: SY.admin's is 180 s, for a teardown or a retire on a spoke.
pub const KILL_WAIT: Duration = Duration::from_secs(190);

/// SY.admin's own hive, where every identity and vault action goes.
pub const MOTHERBEE: &str = "motherbee";

/// A node as SY.admin's `get_node_status` sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodePresence {
    /// `NODE_NOT_FOUND`: no such node on its hive.
    Absent,
    /// The node is there, in this lifecycle state (`RUNNING`, `STOPPED`, ...).
    Present(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeObserved {
    pub presence: NodePresence,
    /// Its ILK in the identity SHM, once registered.
    pub ilk: Option<IlkView>,
}

/// What runs, as SY.deploy reads it before it builds the steps (§5.2): the ILKs, the document's
/// hash, the relations and the definitions' hashes in the SHM; the assets on disk; the nodes
/// through `get_node_status`; the assignments through `vault_list`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    pub person: Option<IlkView>,
    pub nodes: BTreeMap<String, NodeObserved>,
    pub assets_present: BTreeSet<String>,
    /// Each key's assigned ILKs.
    pub vault_assigned: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepKind {
    CreatePerson,
    SetDocument,
    /// Writes the node's assets that are not on disk.
    WriteAssets {
        role: String,
        hashes: Vec<String>,
    },
    RunNode {
        role: String,
    },
    StartNode {
        role: String,
    },
    SetWorksFor {
        role: String,
    },
    SetDefinition {
        role: String,
    },
    VaultAssign {
        key: String,
    },
    /// The record goes `active`.
    RecordActive,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    /// Its name in the journal and in a failure.
    pub label: String,
    /// The SY.admin action it sends; `None` for a step SY.deploy does itself.
    pub call: Option<AdminCall>,
}

fn admin(action: &str, target: &str, params: Value, timeout: Duration) -> Option<AdminCall> {
    Some(AdminCall {
        action: action.to_string(),
        target: target.to_string(),
        params,
        timeout,
    })
}

fn node_hive(node: &DesiredNode) -> &str {
    node.node_name
        .rsplit_once('@')
        .map(|(_, hive)| hive)
        .unwrap_or(MOTHERBEE)
}

/// The steps still missing, in order, the record's last.
pub fn build_steps(desired: &DesiredState, observed: &Observed) -> Vec<Step> {
    let mut steps = Vec::new();
    if let Some(person) = &desired.person {
        let document = Step {
            kind: StepKind::SetDocument,
            label: "set_person_document".to_string(),
            call: admin(
                "set_person_document",
                MOTHERBEE,
                json!({"ilk_id": person.ilk_id, "document": person.document}),
                ACTION_WAIT,
            ),
        };
        match &observed.person {
            None => {
                let mut params = json!({
                    "tenant_id": desired.tenant_id,
                    "display_name": person.display_name,
                    "ilk_id": person.ilk_id,
                });
                if let Some(company) = &person.company_name {
                    params["company_name"] = json!(company);
                }
                steps.push(Step {
                    kind: StepKind::CreatePerson,
                    label: "create_virtual_person".to_string(),
                    call: admin("create_virtual_person", MOTHERBEE, params, ACTION_WAIT),
                });
                steps.push(document);
            }
            Some(ilk) => {
                if ilk.document_hash.as_deref() != Some(person.document_hash16()) {
                    steps.push(document);
                }
            }
        }
    }
    for node in &desired.nodes {
        let seen = observed.nodes.get(&node.role);
        let ilk = seen.and_then(|seen| seen.ilk.as_ref());
        if let Some(definition) = &node.definition {
            let missing: Vec<String> = definition
                .assets()
                .into_iter()
                .map(|asset| asset.hash.clone())
                .filter(|hash| !observed.assets_present.contains(hash))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if !missing.is_empty() {
                steps.push(Step {
                    kind: StepKind::WriteAssets {
                        role: node.role.clone(),
                        hashes: missing,
                    },
                    label: format!("write_assets:{}", node.role),
                    call: None,
                });
            }
        }
        let hive = node_hive(node);
        match seen.map(|seen| &seen.presence) {
            None | Some(NodePresence::Absent) => steps.push(Step {
                kind: StepKind::RunNode {
                    role: node.role.clone(),
                },
                label: format!("run_node:{}", node.role),
                call: admin(
                    "run_node",
                    hive,
                    json!({
                        "node_name": node.node_name,
                        "runtime": node.runtime,
                        "runtime_version": node.runtime_version,
                        "tenant_id": desired.tenant_id,
                        "ilk_id": node.ilk_id,
                        "config": node.config,
                    }),
                    ACTION_WAIT,
                ),
            }),
            Some(NodePresence::Present(lifecycle)) if lifecycle == "STOPPED" => steps.push(Step {
                kind: StepKind::StartNode {
                    role: node.role.clone(),
                },
                label: format!("start_node:{}", node.role),
                call: admin(
                    "start_node",
                    hive,
                    json!({"node_name": node.node_name}),
                    ACTION_WAIT,
                ),
            }),
            Some(NodePresence::Present(_)) => {}
        }
        let related = ilk.is_some_and(|ilk| {
            ilk.works_for.as_deref() == Some(desired.grouping.id.as_str())
                && ilk.work_role.as_deref() == Some(node.work_role.as_str())
        });
        if !related {
            steps.push(Step {
                kind: StepKind::SetWorksFor {
                    role: node.role.clone(),
                },
                label: format!("set_ilk_works_for:{}", node.role),
                call: admin(
                    "set_ilk_works_for",
                    MOTHERBEE,
                    json!({
                        "ilk_id": node.ilk_id,
                        "works_for": desired.grouping.id,
                        "work_role": node.work_role,
                    }),
                    ACTION_WAIT,
                ),
            });
        }
        if let Some(definition) = &node.definition {
            let hashes = |assets: &[crate::desired::DesiredAsset]| -> Vec<String> {
                assets.iter().map(|asset| asset.hash.clone()).collect()
            };
            let defined = ilk.is_some_and(|ilk| {
                ilk.role_hash.as_deref()
                    == definition.role.as_ref().map(|asset| asset.hash.as_str())
                    && ilk.skill_hashes == hashes(&definition.skills)
                    && ilk.handbook_hashes == hashes(&definition.handbooks)
                    && ilk.personality_hash.as_deref()
                        == definition
                            .personality
                            .as_ref()
                            .map(|asset| asset.hash.as_str())
            });
            if !defined {
                steps.push(Step {
                    kind: StepKind::SetDefinition {
                        role: node.role.clone(),
                    },
                    label: format!("set_ilk_definition:{}", node.role),
                    call: admin(
                        "set_ilk_definition",
                        MOTHERBEE,
                        json!({"ilk_id": node.ilk_id, "definition": definition.definition_value()}),
                        ACTION_WAIT,
                    ),
                });
            }
        }
    }
    for entry in &desired.vault {
        let assigned = observed
            .vault_assigned
            .get(&entry.key)
            .is_some_and(|ilks| ilks.iter().any(|ilk| ilk == &entry.ilk_id));
        if !assigned {
            steps.push(Step {
                kind: StepKind::VaultAssign {
                    key: entry.key.clone(),
                },
                label: format!("vault_assign:{}", entry.node_role),
                call: admin(
                    "vault_assign",
                    MOTHERBEE,
                    json!({
                        "key": entry.key,
                        "tenant_id": desired.tenant_id,
                        "assigned_ilks": [entry.ilk_id],
                    }),
                    ACTION_WAIT,
                ),
            });
        }
    }
    steps.push(Step {
        kind: StepKind::RecordActive,
        label: "record_active".to_string(),
        call: None,
    });
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::desired_state;
    use crate::desired::tests::{plain_record, CHANNEL, CONSCIOUS, PERSON};
    use crate::source::tests::plain_ed;

    fn desired() -> DesiredState {
        desired_state(&plain_ed(), &plain_record()).expect("desired")
    }

    fn labels(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|step| step.label.clone()).collect()
    }

    /// Everything the plain ED's deployment makes, as the SHM, the disk, the nodes and the vault
    /// show it once it is done.
    fn all_done(desired: &DesiredState) -> Observed {
        let person = desired.person.as_ref().expect("person");
        let definition = desired.nodes[0].definition.as_ref().expect("definition");
        let node_ilk = |node: &DesiredNode, defined: bool| IlkView {
            ilk_id: node.ilk_id.clone(),
            ilk_type: "agent".to_string(),
            works_for: Some(PERSON.to_string()),
            work_role: Some(node.work_role.clone()),
            role_hash: defined.then(|| definition.role.as_ref().expect("role").hash.clone()),
            handbook_hashes: if defined {
                definition
                    .handbooks
                    .iter()
                    .map(|asset| asset.hash.clone())
                    .collect()
            } else {
                Vec::new()
            },
            personality_hash: defined.then(|| {
                definition
                    .personality
                    .as_ref()
                    .expect("personality")
                    .hash
                    .clone()
            }),
            ..IlkView::default()
        };
        Observed {
            person: Some(IlkView {
                ilk_id: PERSON.to_string(),
                ilk_type: "human".to_string(),
                is_virtual: true,
                document_hash: Some(person.document_hash16().to_string()),
                ..IlkView::default()
            }),
            nodes: BTreeMap::from([
                (
                    "conscious".to_string(),
                    NodeObserved {
                        presence: NodePresence::Present("RUNNING".to_string()),
                        ilk: Some(node_ilk(&desired.nodes[0], true)),
                    },
                ),
                (
                    "channel".to_string(),
                    NodeObserved {
                        presence: NodePresence::Present("RUNNING".to_string()),
                        ilk: Some(node_ilk(&desired.nodes[1], false)),
                    },
                ),
            ]),
            assets_present: definition
                .assets()
                .iter()
                .map(|asset| asset.hash.clone())
                .collect(),
            vault_assigned: BTreeMap::from([(
                "acme-laura-wapp".to_string(),
                vec![CHANNEL.to_string()],
            )]),
        }
    }

    #[test]
    fn from_nothing_the_plain_ed_takes_its_ten_steps_in_order() {
        let desired = desired();
        let steps = build_steps(&desired, &Observed::default());
        assert_eq!(
            labels(&steps),
            [
                "create_virtual_person",
                "set_person_document",
                "write_assets:conscious",
                "run_node:conscious",
                "set_ilk_works_for:conscious",
                "set_ilk_definition:conscious",
                "run_node:channel",
                "set_ilk_works_for:channel",
                "vault_assign:channel",
                "record_active",
            ]
        );
        let call = |n: usize| steps[n].call.clone().expect("an admin call");
        let create = call(0);
        assert_eq!(create.target, "motherbee");
        assert_eq!(
            create.params,
            json!({
                "tenant_id": desired.tenant_id,
                "display_name": "Laura Gómez",
                "company_name": "Acme",
                "ilk_id": PERSON,
            })
        );
        assert_eq!(call(1).params["ilk_id"], json!(PERSON));
        assert_eq!(call(1).params["document"]["short_id"], json!("ed4qm7k2xa"));
        let StepKind::WriteAssets { hashes, .. } = &steps[2].kind else {
            panic!("assets")
        };
        assert_eq!(hashes.len(), 3);
        let run = call(3);
        assert_eq!(run.target, "worker1");
        assert_eq!(run.timeout, ACTION_WAIT);
        assert_eq!(
            run.params,
            json!({
                "node_name": "AI.ed4qm7k2xa@worker1",
                "runtime": "ai.generic",
                "runtime_version": "0.1.159",
                "tenant_id": desired.tenant_id,
                "ilk_id": CONSCIOUS,
                "config": {"behavior": {"kind": "ai_chat", "model": "claude-sonnet-5-5"}, "speaks_as": PERSON},
            })
        );
        assert_eq!(
            call(4).params,
            json!({"ilk_id": CONSCIOUS, "works_for": PERSON, "work_role": "conscious"})
        );
        assert_eq!(call(5).params["definition"]["skill_hashes"], json!([]));
        assert_eq!(
            call(6).params["config"]["io"]["dst_node"],
            json!("AI.ed4qm7k2xa@worker1")
        );
        assert_eq!(
            call(7).params,
            json!({"ilk_id": CHANNEL, "works_for": PERSON, "work_role": "channel"})
        );
        assert_eq!(
            call(8).params,
            json!({"key": "acme-laura-wapp", "tenant_id": desired.tenant_id, "assigned_ilks": [CHANNEL]})
        );
        assert!(steps[9].call.is_none());
    }

    #[test]
    fn what_runs_already_is_not_sent_again() {
        let desired = desired();
        let done = all_done(&desired);
        assert_eq!(labels(&build_steps(&desired, &done)), ["record_active"]);

        // The person and its document there, the conscious launched but not related, nothing else.
        let mut partial = done.clone();
        partial
            .nodes
            .get_mut("conscious")
            .expect("conscious")
            .ilk
            .as_mut()
            .expect("ilk")
            .works_for = None;
        partial
            .nodes
            .get_mut("conscious")
            .expect("conscious")
            .ilk
            .as_mut()
            .expect("ilk")
            .role_hash = None;
        partial.nodes.remove("channel");
        partial.vault_assigned.clear();
        assert_eq!(
            labels(&build_steps(&desired, &partial)),
            [
                "set_ilk_works_for:conscious",
                "set_ilk_definition:conscious",
                "run_node:channel",
                "set_ilk_works_for:channel",
                "vault_assign:channel",
                "record_active",
            ]
        );

        // A document of another version is written again; a stopped node is started, not
        // launched; a missing asset is written alone.
        let mut drift = done.clone();
        drift.person.as_mut().expect("person").document_hash = Some("0000000000000000".to_string());
        drift.nodes.get_mut("channel").expect("channel").presence =
            NodePresence::Present("STOPPED".to_string());
        let personality = desired.nodes[0]
            .definition
            .as_ref()
            .and_then(|definition| definition.personality.as_ref())
            .expect("personality")
            .hash
            .clone();
        drift.assets_present.remove(&personality);
        let steps = build_steps(&desired, &drift);
        assert_eq!(
            labels(&steps),
            [
                "set_person_document",
                "write_assets:conscious",
                "start_node:channel",
                "record_active"
            ]
        );
        assert_eq!(
            steps[1].kind,
            StepKind::WriteAssets {
                role: "conscious".to_string(),
                hashes: vec![personality]
            }
        );

        // A relation to another person, or in another role, is set again.
        let mut other = done;
        other
            .nodes
            .get_mut("channel")
            .expect("channel")
            .ilk
            .as_mut()
            .expect("ilk")
            .work_role = Some("agent".to_string());
        assert_eq!(
            labels(&build_steps(&desired, &other)),
            ["set_ilk_works_for:channel", "record_active"]
        );
    }
}
