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
        .or_else(|| {
            legacy_env_keys
                .iter()
                .find_map(|key| env_non_empty(key))
        })
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
