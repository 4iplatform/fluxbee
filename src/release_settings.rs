//! The settings a software release defines for each hive role (docs/hive-config-update-v1.md §3):
//! the SY nodes a role runs, in their start order, with the ones its start waits for; and the
//! blob's and dist's topology.
//!
//! They live here, in the code, and reach a hive with the binaries that read them: a list and its
//! binaries arrive together, and a core rollback takes both back. They used to live in each hive's
//! `/etc/fluxbee/hive.yaml`, which a spoke's join writes once and nothing writes again, so a
//! release's change to them never reached a hive that had joined before it. That file keeps the
//! hive's own: its identity, its control path, its local paths and ports (§2).
//!
//! The values are the ones every role ran with when they moved here: `system_nodes` of
//! `packaging/hive.yaml.example`, and the blob's and dist's flags the joins wrote (the golden
//! tests below pin both).

/// A hive's role: `role:` in its hive.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HiveRole {
    Motherbee,
    Worker,
    Egress,
    Ingress,
}

impl HiveRole {
    pub const ALL: [HiveRole; 4] = [
        HiveRole::Motherbee,
        HiveRole::Worker,
        HiveRole::Egress,
        HiveRole::Ingress,
    ];

    /// The role hive.yaml's `role:` names, trimmed and in any case; None for no role or another.
    pub fn from_role(role: Option<&str>) -> Option<HiveRole> {
        let role = role?.trim().to_ascii_lowercase();
        HiveRole::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == role)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            HiveRole::Motherbee => "motherbee",
            HiveRole::Worker => "worker",
            HiveRole::Egress => "egress",
            HiveRole::Ingress => "ingress",
        }
    }

    /// What this release defines for the role.
    pub fn settings(self) -> &'static RoleSettings {
        match self {
            HiveRole::Motherbee => &MOTHERBEE,
            HiveRole::Worker => &WORKER,
            HiveRole::Egress => &EGRESS,
            HiveRole::Ingress => &INGRESS,
        }
    }
}

/// What a release defines for one role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleSettings {
    pub system_nodes: SystemNodes,
    pub blob: BlobTopology,
    pub dist: DistTopology,
}

/// The SY nodes a role runs. Only SY.* system authorities: the managed IO/AI/WF runtimes are not
/// lifecycle nodes, the orchestrator spawns them through `run_node`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemNodes {
    /// The start order (Model D'): SY.config.routes first, the writer of the routing SHM every
    /// other node needs; on the motherbee every vault consumer before SY.vault, which comes last,
    /// since the broadcasts SY.vault sends at its start reach only the nodes already registered.
    pub nodes: &'static [&'static str],
    /// The ones the orchestrator's start waits for before it goes on.
    pub wait_for: &'static [&'static str],
}

/// The blob's topology on a role. Its path and Syncthing's API port and home are the hive's own,
/// in hive.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobTopology {
    /// The blob store on this hive.
    pub enabled: bool,
    /// The `fluxbee-blob` folder (`blob/active`), shared through `sync_tool`.
    pub sync_enabled: bool,
    /// The `fluxbee-blob-public` folder (`blob/public`): the motherbee sends, the ingress receives.
    pub public_sync_enabled: bool,
    pub sync_tool: &'static str,
}

/// Dist's topology on a role. Its path is the hive's own, in hive.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistTopology {
    /// The dist folders, shared through `sync_tool`.
    pub sync_enabled: bool,
    pub sync_tool: &'static str,
}

const SYNCTHING: &str = "syncthing";

const MOTHERBEE: RoleSettings = RoleSettings {
    system_nodes: SystemNodes {
        nodes: &[
            "SY.config.routes",
            "SY.identity",
            "SY.opa.rules",
            "SY.admin",
            "SY.architect",
            "SY.storage",
            "SY.cognition",
            "SY.timer",
            "SY.wf-rules",
            "SY.frontdesk.gov",
            "SY.vault",
        ],
        wait_for: &[
            "SY.config.routes",
            "SY.identity",
            "SY.opa.rules",
            "SY.admin",
            "SY.cognition",
            "SY.timer",
            "SY.vault",
        ],
    },
    blob: BlobTopology {
        enabled: true,
        sync_enabled: true,
        public_sync_enabled: true,
        sync_tool: SYNCTHING,
    },
    dist: DistTopology {
        sync_enabled: true,
        sync_tool: SYNCTHING,
    },
};

const WORKER: RoleSettings = RoleSettings {
    system_nodes: SystemNodes {
        nodes: &[
            "SY.config.routes",
            "SY.identity",
            "SY.opa.rules",
            "SY.cognition",
            "SY.timer",
            "SY.wf-rules",
        ],
        wait_for: &[
            "SY.config.routes",
            "SY.identity",
            "SY.opa.rules",
            "SY.cognition",
            "SY.timer",
        ],
    },
    blob: BlobTopology {
        enabled: true,
        sync_enabled: true,
        public_sync_enabled: false,
        sync_tool: SYNCTHING,
    },
    dist: DistTopology {
        sync_enabled: true,
        sync_tool: SYNCTHING,
    },
};

/// The egress runs no blob: it receives its core and the vendor through dist, and the policy, which
/// SY.opa.rules installs here as on every role (the router of each hive enforces the one global
/// user policy).
const EGRESS: RoleSettings = RoleSettings {
    system_nodes: SystemNodes {
        nodes: &["SY.config.routes", "SY.opa.rules"],
        wait_for: &["SY.config.routes"],
    },
    blob: BlobTopology {
        enabled: false,
        sync_enabled: false,
        public_sync_enabled: false,
        sync_tool: SYNCTHING,
    },
    dist: DistTopology {
        sync_enabled: true,
        sync_tool: SYNCTHING,
    },
};

/// The ingress is the public door: SY.edge binds :443 and serves `blob/public`, which it receives
/// from the motherbee. No SY.identity there (the identity frontier): the edge forwards by owner
/// names resolved before.
const INGRESS: RoleSettings = RoleSettings {
    system_nodes: SystemNodes {
        nodes: &["SY.config.routes", "SY.opa.rules", "SY.edge"],
        wait_for: &["SY.config.routes"],
    },
    blob: BlobTopology {
        enabled: false,
        sync_enabled: true,
        public_sync_enabled: true,
        sync_tool: SYNCTHING,
    },
    dist: DistTopology {
        sync_enabled: true,
        sync_tool: SYNCTHING,
    },
};

/// The systemd service of a lifecycle node: `SY.frontdesk.gov` runs as `sy-frontdesk-gov`.
pub fn service_name(node_name: &str) -> String {
    let trimmed = node_name.trim();
    let base = trimmed.strip_prefix("SY.").unwrap_or(trimmed);
    format!("sy-{}", base.to_ascii_lowercase().replace('.', "-"))
}

/// The vault consumers among the motherbee's nodes, which start before SY.vault.
const VAULT_CONSUMERS: &[&str] = &[
    "SY.admin",
    "SY.architect",
    "SY.storage",
    "SY.identity",
    "SY.cognition",
    "SY.frontdesk.gov",
];

/// The rules a role's list keeps, the ones the orchestrator once checked in each hive.yaml at its
/// start: SY.config.routes first; on the motherbee SY.vault present and after every vault
/// consumer, and on no other role; SY.* nodes only, each with a valid service of its own;
/// `wait_for` a subset of the nodes. The table is checked against them by the tests.
pub fn check_system_nodes(role: HiveRole, nodes: &[&str], wait_for: &[&str]) -> Result<(), String> {
    let is_motherbee = role == HiveRole::Motherbee;
    let Some(first) = nodes.first() else {
        return Err(format!("{}: the node list is empty", role.as_str()));
    };
    if first.trim() != "SY.config.routes" {
        return Err(format!(
            "{}: the list must start with SY.config.routes, the writer of the routing SHM every \
             other SY node needs",
            role.as_str()
        ));
    }
    if is_motherbee {
        let Some(vault) = nodes.iter().position(|node| node.trim() == "SY.vault") else {
            return Err(
                "motherbee: the list must include SY.vault, the secret backend".to_string(),
            );
        };
        for consumer in VAULT_CONSUMERS {
            if nodes
                .iter()
                .position(|node| node.trim() == *consumer)
                .is_some_and(|at| at > vault)
            {
                return Err(format!(
                    "motherbee: '{consumer}' comes after SY.vault; a vault consumer starts before \
                     it, so the broadcasts SY.vault sends at its start reach it (router \
                     broadcasts reach only registered nodes)"
                ));
            }
        }
    }
    let mut seen_nodes = std::collections::HashSet::new();
    let mut seen_services = std::collections::HashSet::new();
    for raw in nodes {
        let name = raw.trim();
        // The managed IO/AI/WF runtimes are spawned through run_node, never listed here.
        if !name.starts_with("SY.") {
            return Err(format!(
                "{}: lifecycle node '{name}' is not an SY.* system authority",
                role.as_str()
            ));
        }
        let service = service_name(name);
        let token_ok = service.len() > "sy-".len()
            && service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
        if !token_ok {
            return Err(format!(
                "{}: node '{name}' yields an invalid service '{service}'",
                role.as_str()
            ));
        }
        if !is_motherbee && service == "sy-vault" {
            return Err(format!(
                "{}: SY.vault runs only on the motherbee",
                role.as_str()
            ));
        }
        if !seen_nodes.insert(name) {
            return Err(format!("{}: duplicate node '{name}'", role.as_str()));
        }
        if !seen_services.insert(service.clone()) {
            return Err(format!("{}: duplicate service '{service}'", role.as_str()));
        }
    }
    let mut seen_waits = std::collections::HashSet::new();
    for raw in wait_for {
        let name = raw.trim();
        if !seen_nodes.contains(name) {
            return Err(format!(
                "{}: wait_for names '{name}', which the node list does not",
                role.as_str()
            ));
        }
        if !seen_waits.insert(name) {
            return Err(format!("{}: duplicate wait_for '{name}'", role.as_str()));
        }
    }
    Ok(())
}

/// The keys a hive.yaml held for what the release defines now. An existing file that still holds
/// them parses as before, and they are ignored: the orchestrator names the ones it finds once, at
/// its start. The join and `packaging/hive.yaml.example` no longer write them.
pub const SUPERSEDED_HIVE_YAML_KEYS: [&str; 7] = [
    "system_nodes",
    "blob.enabled",
    "blob.sync.enabled",
    "blob.sync.public_enabled",
    "blob.sync.tool",
    "dist.sync.enabled",
    "dist.sync.tool",
];

/// Which of `SUPERSEDED_HIVE_YAML_KEYS` a parsed hive.yaml holds, in that order.
pub fn superseded_hive_yaml_keys(hive: &serde_yaml::Value) -> Vec<&'static str> {
    SUPERSEDED_HIVE_YAML_KEYS
        .into_iter()
        .filter(|key| {
            key.split('.')
                .try_fold(hive, |value, part| value.get(part))
                .is_some()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `system_nodes` as `packaging/hive.yaml.example` shipped it until the lists moved into the
    /// code, comments aside. All four hives of 8.x ran these lists then.
    const EXAMPLE_SYSTEM_NODES: &str = "
system_nodes:
  motherbee:
    nodes:
      - SY.config.routes
      - SY.identity
      - SY.opa.rules
      - SY.admin
      - SY.architect
      - SY.storage
      - SY.cognition
      - SY.timer
      - SY.wf-rules
      - SY.frontdesk.gov
      - SY.vault
    wait_for:
      - SY.config.routes
      - SY.identity
      - SY.opa.rules
      - SY.admin
      - SY.cognition
      - SY.timer
      - SY.vault
  worker:
    nodes:
      - SY.config.routes
      - SY.identity
      - SY.opa.rules
      - SY.cognition
      - SY.timer
      - SY.wf-rules
    wait_for:
      - SY.config.routes
      - SY.identity
      - SY.opa.rules
      - SY.cognition
      - SY.timer
  egress:
    nodes:
      - SY.config.routes
      - SY.opa.rules
    wait_for:
      - SY.config.routes
  ingress:
    nodes:
      - SY.config.routes
      - SY.opa.rules
      - SY.edge
    wait_for:
      - SY.config.routes
";

    /// The blob and dist sections each hive's hive.yaml got until the flags moved into the code:
    /// the motherbee's from `packaging/hive.yaml.example`, the spokes' as the joins' templates
    /// rendered them on that motherbee (`add_hive_flow`, `add_egress_hive_flow`,
    /// `add_ingress_hive_flow`). The worker's copied the motherbee's flags and named no
    /// `public_enabled`; the egress' wrote its blob off and nothing else of it; the ingress' wrote
    /// its own. The gc and path keys are left out: they stay in hive.yaml.
    const JOINS_BLOB_DIST: [(HiveRole, &str); 4] = [
        (
            HiveRole::Motherbee,
            "
blob:
  enabled: true
  path: \"/var/lib/fluxbee/blob\"
  sync:
    enabled: true
    public_enabled: true
    tool: \"syncthing\"
    api_port: 8384
    data_dir: \"/var/lib/fluxbee/syncthing\"
dist:
  path: \"/var/lib/fluxbee/dist\"
  sync:
    enabled: true
    tool: \"syncthing\"
",
        ),
        (
            HiveRole::Worker,
            "
blob:
  enabled: true
  path: \"/var/lib/fluxbee/blob\"
  sync:
    enabled: true
    tool: \"syncthing\"
    api_port: 8384
    data_dir: \"/var/lib/fluxbee/syncthing\"
dist:
  path: \"/var/lib/fluxbee/dist\"
  sync:
    enabled: true
    tool: \"syncthing\"
",
        ),
        (
            HiveRole::Egress,
            "
blob:
  enabled: false
  sync:
    enabled: false
dist:
  path: \"/var/lib/fluxbee/dist\"
  sync:
    enabled: true
    tool: \"syncthing\"
",
        ),
        (
            HiveRole::Ingress,
            "
blob:
  enabled: false
  path: \"/var/lib/fluxbee/blob\"
  sync:
    enabled: true
    public_enabled: true
    tool: \"syncthing\"
    api_port: 8384
    data_dir: \"/var/lib/fluxbee/syncthing\"
dist:
  path: \"/var/lib/fluxbee/dist\"
  sync:
    enabled: true
    tool: \"syncthing\"
",
        ),
    ];

    fn strings(value: Option<&serde_yaml::Value>) -> Vec<String> {
        value
            .and_then(serde_yaml::Value::as_sequence)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_table_holds_the_system_nodes_the_example_shipped() {
        let shipped: serde_yaml::Value =
            serde_yaml::from_str(EXAMPLE_SYSTEM_NODES).expect("the pinned lists parse");
        for role in HiveRole::ALL {
            let section = &shipped["system_nodes"][role.as_str()];
            let table = role.settings().system_nodes;
            assert_eq!(
                strings(section.get("nodes")),
                table.nodes,
                "{}: nodes",
                role.as_str()
            );
            assert_eq!(
                strings(section.get("wait_for")),
                table.wait_for,
                "{}: wait_for",
                role.as_str()
            );
        }
    }

    /// The flags each hive ran with, read as the orchestrator read them then: a missing
    /// `blob.enabled` was on, a missing `blob.sync.enabled` off, a missing `public_enabled` on for
    /// the motherbee and the ingress only, a missing tool Syncthing, a missing `dist.sync.enabled`
    /// on.
    #[test]
    fn the_table_holds_the_blob_and_dist_flags_the_joins_wrote() {
        for (role, written) in JOINS_BLOB_DIST {
            let hive: serde_yaml::Value =
                serde_yaml::from_str(written).expect("the pinned sections parse");
            let flag = |path: &[&str], default: bool| {
                path.iter()
                    .try_fold(&hive, |value, part| value.get(*part))
                    .and_then(serde_yaml::Value::as_bool)
                    .unwrap_or(default)
            };
            let tool = |path: &[&str]| {
                path.iter()
                    .try_fold(&hive, |value, part| value.get(*part))
                    .and_then(serde_yaml::Value::as_str)
                    .unwrap_or(SYNCTHING)
                    .to_string()
            };
            let public_by_role = matches!(role, HiveRole::Motherbee | HiveRole::Ingress);
            let settings = role.settings();
            assert_eq!(
                (
                    flag(&["blob", "enabled"], true),
                    flag(&["blob", "sync", "enabled"], false),
                    flag(&["blob", "sync", "public_enabled"], public_by_role),
                    tool(&["blob", "sync", "tool"]),
                ),
                (
                    settings.blob.enabled,
                    settings.blob.sync_enabled,
                    settings.blob.public_sync_enabled,
                    settings.blob.sync_tool.to_string(),
                ),
                "{}: blob",
                role.as_str()
            );
            assert_eq!(
                (
                    flag(&["dist", "sync", "enabled"], true),
                    tool(&["dist", "sync", "tool"]),
                ),
                (
                    settings.dist.sync_enabled,
                    settings.dist.sync_tool.to_string()
                ),
                "{}: dist",
                role.as_str()
            );
        }
    }

    /// The rules the orchestrator checked each hive.yaml's lists against at its start, now held by
    /// the table itself.
    #[test]
    fn every_role_of_the_table_keeps_the_boot_order_rules() {
        for role in HiveRole::ALL {
            let nodes = role.settings().system_nodes;
            check_system_nodes(role, nodes.nodes, nodes.wait_for)
                .unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(nodes.nodes.first(), Some(&"SY.config.routes"));
            assert!(
                nodes.nodes.contains(&"SY.opa.rules"),
                "{}: the router of every hive enforces the global user policy",
                role.as_str()
            );
            if role == HiveRole::Motherbee {
                assert_eq!(nodes.nodes.last(), Some(&"SY.vault"));
            } else {
                assert!(!nodes.nodes.contains(&"SY.vault"), "{}", role.as_str());
            }
        }
    }

    #[test]
    fn the_boot_order_rules_refuse_what_they_refused_in_hive_yaml() {
        use HiveRole::*;
        let refused: [(HiveRole, &[&str], &[&str]); 10] = [
            (Egress, &["SY.config.routes", "SY.vault"], &[]),
            (Worker, &["SY.config.routes", "IO.blob"], &[]),
            (
                Motherbee,
                &["SY.config.routes", "SY.admin", "IO.blob", "SY.vault"],
                &[],
            ),
            (
                Motherbee,
                &["SY.config.routes", "IO.cloud", "SY.vault"],
                &[],
            ),
            (Worker, &["SY.identity", "SY.config.routes"], &[]),
            (Motherbee, &["SY.config.routes", "SY.admin"], &[]),
            (
                Motherbee,
                &["SY.config.routes", "SY.vault", "SY.storage"],
                &[],
            ),
            (Worker, &["SY.config.routes", "SY.timer", "SY.timer"], &[]),
            (Egress, &["SY.config.routes"], &["SY.opa.rules"]),
            (Ingress, &[], &[]),
        ];
        for (role, nodes, wait_for) in refused {
            assert!(
                check_system_nodes(role, nodes, wait_for).is_err(),
                "{}: {nodes:?} / {wait_for:?} must be refused",
                role.as_str()
            );
        }
        assert!(check_system_nodes(Egress, &["SY.config.routes"], &[]).is_ok());
        assert!(check_system_nodes(
            Ingress,
            &["SY.config.routes", "SY.edge"],
            &["SY.config.routes"]
        )
        .is_ok());
    }

    #[test]
    fn a_role_and_a_service_are_named_as_before() {
        assert_eq!(
            HiveRole::from_role(Some(" Worker ")),
            Some(HiveRole::Worker)
        );
        assert_eq!(
            HiveRole::from_role(Some("motherbee")),
            Some(HiveRole::Motherbee)
        );
        assert_eq!(HiveRole::from_role(Some("edge")), None);
        assert_eq!(HiveRole::from_role(None), None);
        assert_eq!(service_name("SY.frontdesk.gov"), "sy-frontdesk-gov");
        assert_eq!(service_name(" SY.config.routes "), "sy-config-routes");
    }

    #[test]
    fn the_superseded_keys_are_found_where_a_hive_yaml_holds_them() {
        let old: serde_yaml::Value = serde_yaml::from_str(&format!(
            "hive_id: worker1\nrole: worker\n{}{}",
            JOINS_BLOB_DIST[1].1, EXAMPLE_SYSTEM_NODES
        ))
        .expect("an old worker hive.yaml");
        assert_eq!(
            superseded_hive_yaml_keys(&old),
            vec![
                "system_nodes",
                "blob.enabled",
                "blob.sync.enabled",
                "blob.sync.tool",
                "dist.sync.enabled",
                "dist.sync.tool",
            ]
        );
        let ingress: serde_yaml::Value =
            serde_yaml::from_str(JOINS_BLOB_DIST[3].1).expect("an old ingress section");
        assert!(superseded_hive_yaml_keys(&ingress).contains(&"blob.sync.public_enabled"));
        let own: serde_yaml::Value = serde_yaml::from_str(
            "hive_id: w\nrole: worker\nblob:\n  path: /b\n  sync:\n    api_port: 8384\ndist:\n  path: /d\n",
        )
        .expect("a hive.yaml of its own");
        assert!(superseded_hive_yaml_keys(&own).is_empty());
    }

    /// The shipped files hold only the hive's own: what the release defines is here.
    #[test]
    fn the_shipped_hive_yaml_files_hold_none_of_the_release_s_keys() {
        for (name, raw) in [
            (
                "packaging/hive.yaml.example",
                include_str!("../packaging/hive.yaml.example"),
            ),
            ("config/hive.yaml", include_str!("../config/hive.yaml")),
        ] {
            let hive: serde_yaml::Value = serde_yaml::from_str(raw).expect(name);
            assert_eq!(
                superseded_hive_yaml_keys(&hive),
                Vec::<&str>::new(),
                "{name}"
            );
            assert!(hive["blob"]["path"].is_string(), "{name}: blob.path stays");
            assert!(hive["dist"]["path"].is_string(), "{name}: dist.path stays");
        }
    }
}
