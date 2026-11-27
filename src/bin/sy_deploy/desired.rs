//! The desired state (docs/digital-executive-v1-stage-2.md §3): a source plus its blanks' values,
//! with every name and id made concrete. It is computed from the deployment's record (its ids, its
//! hive, its pinned runtime versions) and the source's copy, so a restart computes the same.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use json_router::agent_assets::{
    build_handbook_asset_value, build_personality_asset_value, build_role_asset_value,
    build_skill_asset_value, prepare_agent_asset, sha256_hex,
};

use crate::source::{
    asset_ref, canonical_bytes, node_ilk_ref, node_name_ref, resolve_template, ParsedSource,
    REF_GROUPING_ILK, REF_GROUPING_SHORT_ID, REF_HIVE, REF_TENANT,
};
use crate::store::{DeploymentRecord, GroupingRef, RecordAssets, RecordVault};

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredState {
    pub deployment_id: String,
    pub tenant_id: String,
    pub hive: String,
    pub grouping: GroupingRef,
    /// The person the deployment founds, when it founds one.
    pub person: Option<DesiredPerson>,
    pub nodes: Vec<DesiredNode>,
    pub vault: Vec<DesiredVault>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredPerson {
    pub ilk_id: String,
    pub display_name: String,
    pub company_name: Option<String>,
    pub document: Value,
    /// The SHA-256 of the document's canonical bytes, as SY.identity takes it; the SHM holds its
    /// first 16 hex.
    pub document_hash: String,
}

impl DesiredPerson {
    pub fn document_hash16(&self) -> &str {
        &self.document_hash[..16]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredNode {
    pub role: String,
    pub kind: String,
    pub node_name: String,
    pub ilk_id: String,
    pub runtime: String,
    pub runtime_version: String,
    pub work_role: String,
    pub config: Value,
    pub definition: Option<DesiredDefinition>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesiredDefinition {
    pub role: Option<DesiredAsset>,
    pub skills: Vec<DesiredAsset>,
    pub handbooks: Vec<DesiredAsset>,
    pub personality: Option<DesiredAsset>,
}

impl DesiredDefinition {
    pub fn assets(&self) -> Vec<&DesiredAsset> {
        self.role
            .iter()
            .chain(self.skills.iter())
            .chain(self.handbooks.iter())
            .chain(self.personality.iter())
            .collect()
    }

    /// The definition `set_ilk_definition` takes.
    pub fn definition_value(&self) -> Value {
        json!({
            "role_hash": self.role.as_ref().map(|asset| asset.hash.clone()),
            "skill_hashes": self.skills.iter().map(|asset| asset.hash.clone()).collect::<Vec<_>>(),
            "handbook_hashes": self.handbooks.iter().map(|asset| asset.hash.clone()).collect::<Vec<_>>(),
            "personality_hash": self.personality.as_ref().map(|asset| asset.hash.clone()),
        })
    }

    pub fn record(&self) -> RecordAssets {
        RecordAssets {
            role: self.role.as_ref().map(|asset| asset.hash.clone()),
            skills: self.skills.iter().map(|asset| asset.hash.clone()).collect(),
            handbooks: self
                .handbooks
                .iter()
                .map(|asset| asset.hash.clone())
                .collect(),
            personality: self.personality.as_ref().map(|asset| asset.hash.clone()),
        }
    }
}

/// An agent asset, its canonical form and the 64 hex of its hash, which names its file.
#[derive(Debug, Clone, PartialEq)]
pub struct DesiredAsset {
    pub hash: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DesiredVault {
    pub key: String,
    pub resource_type: String,
    pub node_role: String,
    pub node_name: String,
    pub ilk_id: String,
}

/// An asset in the form Archi writes it: through its type's builder, then checked and hashed.
fn desired_asset(
    template: &Value,
    blanks: &Value,
    refs: &BTreeMap<String, Value>,
) -> Result<DesiredAsset, String> {
    let resolved = resolve_template(template, blanks, refs)?;
    let normalized = match resolved.get("asset_type").and_then(Value::as_str) {
        Some("role") => build_role_asset_value(&resolved),
        Some("skill") => build_skill_asset_value(&resolved),
        Some("handbook") => build_handbook_asset_value(&resolved),
        Some("personality") => build_personality_asset_value(&resolved),
        _ => {
            return Err(
                "an asset names its asset_type: role, skill, handbook or personality".to_string(),
            )
        }
    }
    .map_err(|err| err.to_string())?;
    let prepared = prepare_agent_asset(&normalized).map_err(|err| err.to_string())?;
    Ok(DesiredAsset {
        hash: prepared.hash,
        value: normalized,
    })
}

fn text(value: Value, what: &str) -> Result<String, String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{what} resolves to a non-empty string"))
}

/// The desired state of `record`'s deployment of `parsed`. Its errors name what does not resolve:
/// a blank whose value does not make a valid asset, or a record that lacks a node of the source.
pub fn desired_state(
    parsed: &ParsedSource,
    record: &DeploymentRecord,
) -> Result<DesiredState, String> {
    let source = &parsed.source;
    let blanks = &record.blanks;
    let mut refs: BTreeMap<String, Value> = BTreeMap::new();
    refs.insert(REF_GROUPING_ILK.to_string(), json!(record.grouping.id));
    refs.insert(
        REF_GROUPING_SHORT_ID.to_string(),
        json!(record.grouping.short_id),
    );
    refs.insert(REF_HIVE.to_string(), json!(record.hive));
    refs.insert(REF_TENANT.to_string(), json!(record.tenant_id));
    for node in &source.nodes {
        let made = record
            .node(&node.role)
            .ok_or_else(|| format!("the record holds no node of the role '{}'", node.role))?;
        refs.insert(node_name_ref(&node.role), json!(made.node_name));
        refs.insert(node_ilk_ref(&node.role), json!(made.ilk_id));
    }
    // The assets first: the person's document and the configs may name them.
    let mut definitions: BTreeMap<String, DesiredDefinition> = BTreeMap::new();
    for node in &source.nodes {
        let Some(spec) = &node.definition else {
            continue;
        };
        let make = |template: &Value| desired_asset(template, blanks, &refs);
        let definition = DesiredDefinition {
            role: spec.role.as_ref().map(make).transpose()?,
            skills: spec.skills.iter().map(make).collect::<Result<_, _>>()?,
            handbooks: spec.handbooks.iter().map(make).collect::<Result<_, _>>()?,
            personality: spec.personality.as_ref().map(make).transpose()?,
        };
        if let Some(role) = &definition.role {
            refs.insert(
                asset_ref(&node.role, "role"),
                json!(format!("sha256:{}", role.hash)),
            );
        }
        if let Some(personality) = &definition.personality {
            refs.insert(
                asset_ref(&node.role, "personality"),
                json!(format!("sha256:{}", personality.hash)),
            );
        }
        definitions.insert(node.role.clone(), definition);
    }
    let person = match &source.founds {
        Some(founds) => {
            let display_name = text(
                resolve_template(&founds.person.display_name, blanks, &refs)?,
                "the person's display_name",
            )?;
            let company_name = founds
                .person
                .company_name
                .as_ref()
                .map(|template| {
                    resolve_template(template, blanks, &refs)
                        .and_then(|value| text(value, "the person's company_name"))
                })
                .transpose()?;
            let document = resolve_template(&founds.document, blanks, &refs)?;
            let document_hash = sha256_hex(&canonical_bytes(&document));
            Some(DesiredPerson {
                ilk_id: record.grouping.id.clone(),
                display_name,
                company_name,
                document,
                document_hash,
            })
        }
        None => None,
    };
    let mut nodes = Vec::with_capacity(source.nodes.len());
    for node in &source.nodes {
        let made = record
            .node(&node.role)
            .ok_or_else(|| format!("the record holds no node of the role '{}'", node.role))?;
        nodes.push(DesiredNode {
            role: node.role.clone(),
            kind: node.kind.clone(),
            node_name: made.node_name.clone(),
            ilk_id: made.ilk_id.clone(),
            runtime: node.runtime.clone(),
            runtime_version: made.runtime_version.clone(),
            work_role: node.work_role.clone(),
            config: resolve_template(&node.config, blanks, &refs)?,
            definition: definitions.remove(&node.role),
        });
    }
    let mut vault = Vec::with_capacity(source.vault.len());
    for entry in &source.vault {
        let key = text(
            resolve_template(&entry.key, blanks, &refs)?,
            "a vault entry's key",
        )?;
        let node = nodes
            .iter()
            .find(|node| node.role == entry.to)
            .ok_or_else(|| {
                format!(
                    "a vault entry goes to the role '{}', which has no node",
                    entry.to
                )
            })?;
        vault.push(DesiredVault {
            key,
            resource_type: entry.resource_type.clone(),
            node_role: node.role.clone(),
            node_name: node.node_name.clone(),
            ilk_id: node.ilk_id.clone(),
        });
    }
    Ok(DesiredState {
        deployment_id: record.deployment_id.clone(),
        tenant_id: record.tenant_id.clone(),
        hive: record.hive.clone(),
        grouping: record.grouping.clone(),
        person,
        nodes,
        vault,
    })
}

/// What a record keeps of a desired state: each agent's assets and the keys it assigns.
pub fn record_parts(desired: &DesiredState) -> (BTreeMap<String, RecordAssets>, Vec<RecordVault>) {
    let assets = desired
        .nodes
        .iter()
        .filter_map(|node| {
            node.definition
                .as_ref()
                .map(|definition| (node.role.clone(), definition.record()))
        })
        .collect();
    let vault = desired
        .vault
        .iter()
        .map(|entry| RecordVault {
            key: entry.key.clone(),
            resource_type: entry.resource_type.clone(),
            assigned_to: entry.node_name.clone(),
        })
        .collect();
    (assets, vault)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::source::tests::{plain_blanks, plain_ed};
    use crate::store::{DeploymentState, RecordNode, SourceRef};

    pub const PERSON: &str = "ilk:550e8400-e29b-41d4-a716-446655440001";
    pub const CONSCIOUS: &str = "ilk:550e8400-e29b-41d4-a716-446655440002";
    pub const CHANNEL: &str = "ilk:550e8400-e29b-41d4-a716-446655440003";

    pub fn plain_record() -> DeploymentRecord {
        let parsed = plain_ed();
        DeploymentRecord {
            deployment_id: "dp7xk3m2qa".to_string(),
            grouping: GroupingRef {
                kind: "ed".to_string(),
                id: PERSON.to_string(),
                short_id: "ed4qm7k2xa".to_string(),
            },
            tenant_id: crate::world::fake::TENANT.to_string(),
            request_key: "cloud-ed-7f3a".to_string(),
            request_hash: "x".to_string(),
            source: SourceRef {
                id: "plain-ed".to_string(),
                version: "1".to_string(),
                hash: parsed.hash.clone(),
            },
            founding: true,
            blanks: plain_blanks(),
            hive: "worker1".to_string(),
            nodes: vec![
                RecordNode {
                    role: "conscious".to_string(),
                    node_name: "AI.ed4qm7k2xa@worker1".to_string(),
                    ilk_id: CONSCIOUS.to_string(),
                    runtime: "ai.generic".to_string(),
                    runtime_version: "0.1.159".to_string(),
                    work_role: "conscious".to_string(),
                },
                RecordNode {
                    role: "channel".to_string(),
                    node_name: "IO.wapp.ed4qm7k2xa@worker1".to_string(),
                    ilk_id: CHANNEL.to_string(),
                    runtime: "io.wapp".to_string(),
                    runtime_version: "0.1.159".to_string(),
                    work_role: "channel".to_string(),
                },
            ],
            assets: BTreeMap::new(),
            vault: Vec::new(),
            state: DeploymentState::Deploying,
            failure: None,
            removal: None,
            requested_by: "cloud".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            history: Vec::new(),
        }
    }

    #[test]
    fn the_plain_ed_s_desired_state_has_every_name_and_id_concrete() {
        let desired = desired_state(&plain_ed(), &plain_record()).expect("desired");
        let person = desired.person.as_ref().expect("a founded person");
        assert_eq!(person.ilk_id, PERSON);
        assert_eq!(person.display_name, "Laura Gómez");
        assert_eq!(person.company_name.as_deref(), Some("Acme"));
        let conscious = &desired.nodes[0];
        let channel = &desired.nodes[1];
        let definition = conscious.definition.as_ref().expect("its definition");
        let personality = definition.personality.as_ref().expect("a personality");
        assert_eq!(
            person.document,
            json!({
                "schema": 1,
                "display_name": "Laura Gómez",
                "company_name": "Acme",
                "short_id": "ed4qm7k2xa",
                "personality": {
                    "asset": format!("sha256:{}", personality.hash),
                    "timezone": "America/Argentina/Buenos_Aires",
                    "country_code": "AR",
                    "primary_language": "es",
                    "additional_languages": ["en"]
                },
                "channels": [{"type": "whatsapp", "address": "1234567890", "io_node": "IO.wapp.ed4qm7k2xa@worker1"}]
            })
        );
        assert_eq!(
            person.document_hash,
            sha256_hex(&canonical_bytes(&person.document))
        );
        assert_eq!(
            conscious.config,
            json!({"behavior": {"kind": "ai_chat", "model": "claude-sonnet-5-5"}, "speaks_as": PERSON})
        );
        assert_eq!(
            channel.config,
            json!({
                "wapp": {"auth": {"type": "vault_ref", "resource_type": "whatsapp", "key": "acme-laura-wapp"}},
                "io": {
                    "phone_number_id": "1234567890",
                    "waba_id": "998877",
                    "dst_node": "AI.ed4qm7k2xa@worker1",
                    "person_ilk": PERSON
                }
            })
        );
        assert!(channel.definition.is_none());
        assert_eq!(definition.handbooks.len(), 1);
        let role = definition.role.as_ref().expect("a role");
        assert!(role.value["description"]
            .as_str()
            .is_some_and(|text| text.starts_with("You are a digital executive of your company.")));
        assert_eq!(personality.value["name"], json!("Laura Gómez"));
        assert_eq!(
            personality.value["system_fields"]["timezone"],
            json!("America/Argentina/Buenos_Aires")
        );
        assert_eq!(personality.value["narrative"], json!({"voice": "calm"}));
        assert_eq!(
            definition.definition_value(),
            json!({
                "role_hash": role.hash,
                "skill_hashes": [],
                "handbook_hashes": [definition.handbooks[0].hash],
                "personality_hash": personality.hash,
            })
        );
        assert_eq!(
            desired.vault,
            [DesiredVault {
                key: "acme-laura-wapp".to_string(),
                resource_type: "whatsapp".to_string(),
                node_role: "channel".to_string(),
                node_name: "IO.wapp.ed4qm7k2xa@worker1".to_string(),
                ilk_id: CHANNEL.to_string(),
            }]
        );
        // The same record always gives the same state: a resume converges on it.
        assert_eq!(
            desired_state(&plain_ed(), &plain_record()).expect("again"),
            desired
        );
    }

    #[test]
    fn the_shared_texts_hash_the_same_for_every_ed_and_the_personality_differs() {
        let laura = desired_state(&plain_ed(), &plain_record()).expect("laura");
        let mut other = plain_record();
        other.blanks["display_name"] = json!("Ana");
        let ana = desired_state(&plain_ed(), &other).expect("ana");
        let def = |state: &DesiredState| state.nodes[0].definition.clone().expect("definition");
        assert_eq!(def(&laura).role, def(&ana).role);
        assert_eq!(def(&laura).handbooks, def(&ana).handbooks);
        assert_ne!(def(&laura).personality, def(&ana).personality);
    }

    #[test]
    fn a_record_without_one_of_the_source_s_nodes_has_no_desired_state() {
        let mut record = plain_record();
        record.nodes.pop();
        assert!(desired_state(&plain_ed(), &record)
            .expect_err("refused")
            .contains("channel"));
    }
}
