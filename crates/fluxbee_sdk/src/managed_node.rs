use std::path::PathBuf;

pub const FLUXBEE_NODE_NAME_ENV: &str = "FLUXBEE_NODE_NAME";
pub const DEFAULT_MANAGED_NODE_ROOT: &str = "/var/lib/fluxbee/nodes";

#[derive(Debug, thiserror::Error)]
pub enum ManagedNodeError {
    #[error("missing hive suffix in node_name '{0}'; expected <name>@<hive>")]
    MissingHive(String),
    #[error("missing kind prefix in node_name '{0}'; expected <KIND>.*")]
    MissingKind(String),
    #[error("invalid empty node_name")]
    EmptyNodeName,
}

pub fn managed_node_name(default_name: &str, legacy_env_keys: &[&str]) -> String {
    env_non_empty(FLUXBEE_NODE_NAME_ENV)
        .or_else(|| legacy_env_keys.iter().find_map(|key| env_non_empty(key)))
        .unwrap_or_else(|| default_name.to_string())
}

pub fn managed_node_instance_dir(node_name: &str) -> Result<PathBuf, ManagedNodeError> {
    managed_node_instance_dir_with_root(node_name, DEFAULT_MANAGED_NODE_ROOT)
}

pub fn managed_node_instance_dir_with_root(
    node_name: &str,
    root: impl Into<PathBuf>,
) -> Result<PathBuf, ManagedNodeError> {
    let node_name = node_name.trim();
    if node_name.is_empty() {
        return Err(ManagedNodeError::EmptyNodeName);
    }
    let (local_name, _) = node_name
        .split_once('@')
        .ok_or_else(|| ManagedNodeError::MissingHive(node_name.to_string()))?;
    let kind = local_name
        .split('.')
        .next()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ManagedNodeError::MissingKind(node_name.to_string()))?;
    Ok(root.into().join(kind).join(node_name))
}

/// Los unicos nodos NO-`SY.*` que `hive.yaml` acepta en su lista de nodos de ciclo de vida.
///
/// **VACÍO a proposito.** Los runtimes managed (io.api, io.blob, io.cloud, io.slack, ...) NO se
/// declaran en `hive.yaml` `system_nodes`: nacen por `run_node` y el orquestador los reconcilia.
pub const HIVE_YAML_NON_SY_LIFECYCLE_NODES: &[&str] = &[];

/// True cuando `hive.yaml` puede listarlo como nodo de ciclo de vida sin ser `SY.*`.
pub fn is_allowed_non_sy_lifecycle_node(node_name: &str) -> bool {
    node_matches(node_name, HIVE_YAML_NON_SY_LIFECYCLE_NODES)
}

fn node_matches(node_name: &str, known: &[&str]) -> bool {
    let local = node_name
        .split_once('@')
        .map(|(local, _)| local)
        .unwrap_or(node_name)
        .trim();
    known.iter().any(|k| k.eq_ignore_ascii_case(local))
}

/// The IO runtimes the core itself runs, in the root tenant: the ones the platform needs as shared
/// resources, not because a solution asks for them (operator decisions 2026-10-02 and 2026-10-03,
/// FINDINGS A-43). io.web is designed (docs/io-web-spec-beta-v1.md) but not built yet. Every other
/// IO runtime stays installed and spawnable, but a tenant launches it: never in the root tenant,
/// never by default.
pub const ROOT_TENANT_IO_RUNTIMES: &[&str] = &["io.cloud", "io.blob", "io.web"];

/// True when the root tenant refuses this node: an IO node (an `IO.*` name or an `io.*` runtime)
/// whose runtime is not one of [`ROOT_TENANT_IO_RUNTIMES`], with `tenant_id` the root tenant.
/// Other kinds of node are not restricted here.
pub fn root_tenant_refuses_io_node(node_name: &str, runtime: &str, tenant_id: &str) -> bool {
    let local = node_name
        .split_once('@')
        .map(|(local, _)| local)
        .unwrap_or(node_name)
        .trim();
    let runtime = runtime.trim();
    let is_io = local.starts_with("IO.") || runtime.to_ascii_lowercase().starts_with("io.");
    is_io
        && is_root_tenant(tenant_id)
        && !ROOT_TENANT_IO_RUNTIMES
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(runtime))
}

/// The root tenant, in any spelling of `tnt:<uuid>` that parses to it (with or without hyphens).
pub fn is_root_tenant(tenant_id: &str) -> bool {
    let uuid_of = |raw: &str| {
        raw.trim()
            .strip_prefix("tnt:")
            .and_then(|uuid| uuid::Uuid::parse_str(uuid).ok())
    };
    match (
        uuid_of(tenant_id),
        uuid_of(crate::identity::DEFAULT_ROOT_TENANT_ID),
    ) {
        (Some(tenant), Some(root)) => tenant == root,
        _ => false,
    }
}

pub fn managed_node_config_path(node_name: &str) -> Result<PathBuf, ManagedNodeError> {
    Ok(managed_node_instance_dir(node_name)?.join("config.json"))
}

pub fn managed_node_config_path_with_root(
    node_name: &str,
    root: impl Into<PathBuf>,
) -> Result<PathBuf, ManagedNodeError> {
    Ok(managed_node_instance_dir_with_root(node_name, root)?.join("config.json"))
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_node_name_prefers_fluxbee_env() {
        unsafe { std::env::set_var(FLUXBEE_NODE_NAME_ENV, "AI.managed@motherbee") };
        unsafe { std::env::set_var("GOV_NODE_NAME", "AI.legacy@motherbee") };
        let name = managed_node_name("AI.default@motherbee", &["GOV_NODE_NAME"]);
        assert_eq!(name, "AI.managed@motherbee");
        unsafe { std::env::remove_var(FLUXBEE_NODE_NAME_ENV) };
        unsafe { std::env::remove_var("GOV_NODE_NAME") };
    }

    /// A-43: only the core IO runtimes (io.cloud, io.blob, io.web) run in the root tenant; any
    /// other IO node is launched by a tenant. Other kinds, and other tenants, are not restricted.
    #[test]
    fn the_root_tenant_takes_only_the_core_io_runtimes() {
        let root = crate::identity::DEFAULT_ROOT_TENANT_ID;
        let root_without_hyphens = "tnt:00000000000000000000000000000001";
        let tenant = "tnt:8a0c3f8e-2f1b-4d3a-9c55-0f6e2b7d9a11";
        assert!(!root_tenant_refuses_io_node(
            "IO.cloud@motherbee",
            "io.cloud",
            root
        ));
        assert!(!root_tenant_refuses_io_node(
            "IO.blob@motherbee",
            "io.blob",
            root
        ));
        assert!(!root_tenant_refuses_io_node(
            "IO.web@motherbee",
            "io.web",
            root
        ));
        for (name, runtime) in [
            ("IO.api@motherbee", "io.api"),
            ("IO.wapp.default@motherbee", "io.wapp"),
            ("IO.slack.acme@motherbee", "io.slack"),
            ("IO.cloud2@motherbee", "io.api"),
            ("IO.anything@motherbee", ""),
            ("WF.oddly.named@motherbee", "io.api"),
        ] {
            assert!(
                root_tenant_refuses_io_node(name, runtime, root),
                "{name} ({runtime})"
            );
            assert!(root_tenant_refuses_io_node(
                name,
                runtime,
                root_without_hyphens
            ));
            assert!(
                !root_tenant_refuses_io_node(name, runtime, tenant),
                "{name} in a tenant"
            );
        }
        assert!(!root_tenant_refuses_io_node(
            "AI.sales@motherbee",
            "ai.generic",
            root
        ));
        assert!(!root_tenant_refuses_io_node(
            "WF.flow@motherbee",
            "wf.generic",
            root
        ));
        assert!(!is_root_tenant("tnt:not-a-uuid"));
        assert!(!is_root_tenant(""));
    }

    #[test]
    fn managed_node_config_path_uses_kind_and_full_name() {
        let path = managed_node_config_path_with_root(
            "WF.demo.test@motherbee",
            "/tmp/fluxbee-managed-test",
        )
        .expect("config path");
        assert_eq!(
            path,
            PathBuf::from("/tmp/fluxbee-managed-test/WF/WF.demo.test@motherbee/config.json")
        );
    }
}
