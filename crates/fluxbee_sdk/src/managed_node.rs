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

/// Singletons que arranca el PAQUETE con su propia unit de systemd.
///
/// `base-nodes.json` los declara con `unit` y `role_gate`; systemd es el dueño de su ciclo de vida.
/// Eso los distingue de los runtimes, que nacen por `run_node` y a los que el orquestador relanza.
///
/// Sirve para UNA sola cosa: que el barrido que relanza nodos persistidos **nunca los adopte**. Si
/// lo hiciera habria dos duenos del mismo proceso y dos procesos con el mismo nombre L2 — y el
/// router entrega al primero que matchea en el FIB, asi que un CONFIG_SET caeria en uno u otro sin
/// determinismo.
///
/// OJO, no confundir con [`HIVE_YAML_NON_SY_LIFECYCLE_NODES`]: ser singleton empaquetado NO
/// habilita a declararse como nodo de ciclo de vida en `hive.yaml`. Son dos permisos distintos y
/// fusionarlos rompe un test que lo fija a proposito.
pub const PACKAGED_SINGLETON_NODES: &[&str] = &["IO.blob", "IO.cloud"];

/// Los unicos nodos NO-`SY.*` que `hive.yaml` acepta en su lista de nodos de ciclo de vida.
///
/// Mas restrictivo que [`PACKAGED_SINGLETON_NODES`] a proposito: `IO.cloud` es un singleton
/// empaquetado pero NO participa del orden de arranque del hive, y declararlo ahi seria darle una
/// responsabilidad que no tiene.
pub const HIVE_YAML_NON_SY_LIFECYCLE_NODES: &[&str] = &["IO.blob"];

/// True cuando el nombre L2 (con o sin `@hive`) es un singleton empaquetado.
pub fn is_packaged_singleton(node_name: &str) -> bool {
    node_matches(node_name, PACKAGED_SINGLETON_NODES)
}

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
