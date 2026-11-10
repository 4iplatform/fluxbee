use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use uuid::Uuid;

const DEFAULT_SHM_PREFIX: &str = "/jsr-";
const DEFAULT_HELLO_INTERVAL_MS: u64 = 10_000;
const DEFAULT_DEAD_INTERVAL_MS: u64 = 40_000;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
const DEFAULT_HEARTBEAT_STALE_MS: u64 = 30_000;
const DEFAULT_HIVE_ROLE: &str = "worker";
const DEFAULT_IDENTITY_FRONTDESK_NODE_BASE: &str = "SY.frontdesk.gov";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing hive.yaml")]
    MissingHive,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
}

#[derive(Clone, Debug)]
pub struct RouterConfig {
    pub router_name: String,
    pub router_l2_name: String,
    pub router_uuid: Uuid,
    pub hive_id: String,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub node_socket_dir: PathBuf,
    pub node_socket_path: PathBuf,
    pub shm_name: String,
    pub shm_prefix: String,
    pub is_gateway: bool,
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub heartbeat_stale_ms: u64,
    pub wan_listen: Option<String>,
    pub wan_uplinks: Vec<String>,
    /// WAN mTLS peer-auth mode: "disabled" | "permissive" | "required".
    pub wan_mtls_mode: String,
    /// Directory holding this hive's mesh TLS material (ca.crt, cert.crt,
    /// cert.key) — `<MESH_TLS_DIR>/<hive_id>`, `/var/lib/fluxbee/tls/<hive_id>`.
    pub tls_dir: PathBuf,
    pub hive_role: String,
    pub identity_frontdesk_node_name: String,
}

/// The keys of hive.yaml the router reads. Every other key is left unparsed, whatever it holds, as
/// an old hive.yaml's `ai` and `blob` are: the readers take `ai` from the installation file since
/// 0.1.121 (docs/hive-config-update-v1.md §4), and the blob's topology lives in the code since
/// 0.1.117 (`release_settings`).
#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    government: Option<GovernmentSection>,
}

#[derive(Debug, Deserialize)]
struct GovernmentSection {
    identity_frontdesk: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    listen: Option<String>,
    uplinks: Option<Vec<WanUplink>>,
    gateway_name: Option<String>,
    /// WAN mTLS peer-auth mode: "disabled" | "permissive" | "required".
    mtls: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WanUplink {
    address: String,
}

#[derive(Debug, Deserialize)]
struct IdentityFile {
    layer1: IdentityLayer1,
    shm: IdentityShm,
}

#[derive(Debug, Deserialize)]
struct IdentityLayer1 {
    uuid: String,
}

#[derive(Debug, Deserialize)]
struct IdentityShm {
    name: String,
}

/// What the router takes from hive.yaml's text.
#[derive(Debug)]
struct HiveSettings {
    hive_id: String,
    hive_role: String,
    gateway_name: String,
    wan_listen: Option<String>,
    wan_uplinks: Vec<String>,
    wan_mtls_mode: String,
    identity_frontdesk_node_name: String,
}

/// The router's settings from hive.yaml's text. Until 0.1.124 it also parsed `ai`, only to check
/// it, and an `ai` no reader could use stopped the router at its start.
fn hive_settings_from(yaml: &str) -> Result<HiveSettings, ConfigError> {
    let hive: HiveFile = serde_yaml::from_str(yaml)?;
    let hive_id = hive.hive_id;
    let mut hive_role = DEFAULT_HIVE_ROLE.to_string();
    if let Some(role) = hive.role {
        let role = role.trim().to_ascii_lowercase();
        if !role.is_empty() {
            hive_role = role;
        }
    }
    let mut gateway_name = "RT.gateway".to_string();
    let mut wan_listen = None;
    let mut wan_uplinks = Vec::new();
    let mut wan_mtls_mode = wan_mtls_mode_from(None).to_string();
    if let Some(wan) = hive.wan {
        wan_listen = wan.listen;
        if let Some(uplinks) = wan.uplinks {
            for uplink in uplinks {
                wan_uplinks.push(uplink.address);
            }
        }
        if let Some(name) = wan.gateway_name {
            gateway_name = name;
        }
        wan_mtls_mode = wan_mtls_mode_from(wan.mtls.as_deref()).to_string();
    }
    let mut identity_frontdesk_node_name =
        format!("{DEFAULT_IDENTITY_FRONTDESK_NODE_BASE}@{hive_id}");
    if let Some(government) = hive.government {
        if let Some(frontdesk) = government.identity_frontdesk {
            let frontdesk = frontdesk.trim();
            if !frontdesk.is_empty() {
                identity_frontdesk_node_name = if frontdesk.contains('@') {
                    frontdesk.to_string()
                } else {
                    format!("{frontdesk}@{hive_id}")
                };
            }
        }
    }
    Ok(HiveSettings {
        hive_id,
        hive_role,
        gateway_name,
        wan_listen,
        wan_uplinks,
        wan_mtls_mode,
        identity_frontdesk_node_name,
    })
}

impl RouterConfig {
    pub fn load_from_env() -> Result<Self, ConfigError> {
        let config_dir = crate::paths::config_dir();
        let state_dir = crate::paths::state_dir();
        let node_socket_dir = crate::paths::router_socket_dir();
        let shm_prefix = DEFAULT_SHM_PREFIX.to_string();
        let hello_interval_ms = DEFAULT_HELLO_INTERVAL_MS;
        let dead_interval_ms = DEFAULT_DEAD_INTERVAL_MS;
        let heartbeat_interval_ms = DEFAULT_HEARTBEAT_INTERVAL_MS;
        let heartbeat_stale_ms = DEFAULT_HEARTBEAT_STALE_MS;

        let hive_path = config_dir.join("hive.yaml");
        if !hive_path.exists() {
            return Err(ConfigError::MissingHive);
        }
        let data = std::fs::read_to_string(&hive_path)?;
        let HiveSettings {
            hive_id,
            hive_role,
            gateway_name,
            wan_listen,
            wan_uplinks,
            wan_mtls_mode,
            identity_frontdesk_node_name,
        } = hive_settings_from(&data)?;

        let router_name = std::env::var("JSR_ROUTER_NAME").unwrap_or_else(|_| gateway_name.clone());
        let is_gateway = is_gateway_name(&router_name, &gateway_name);
        let router_l2_name = ensure_l2_name(&router_name, &hive_id);
        let identity_path = identity_path(&state_dir, &router_l2_name);
        let (router_uuid, shm_name) = if identity_path.exists() {
            let identity_raw = fs::read_to_string(&identity_path)?;
            let identity: IdentityFile = serde_yaml::from_str(&identity_raw)?;
            let uuid = Uuid::parse_str(&identity.layer1.uuid)?;
            (uuid, identity.shm.name)
        } else {
            fs::create_dir_all(identity_path.parent().unwrap_or(Path::new(".")))?;
            let uuid = Uuid::new_v4();
            let shm_name = crate::shm::build_shm_name(&shm_prefix, uuid);
            let identity = build_identity_yaml(&router_l2_name, uuid, &shm_name)?;
            fs::write(&identity_path, identity)?;
            (uuid, shm_name)
        };
        let node_socket_path = node_socket_dir.join(format!("{}.sock", router_uuid.simple()));
        let tls_dir = PathBuf::from(fluxbee_sdk::mesh::MESH_TLS_DIR).join(&hive_id);
        Ok(Self {
            router_name,
            router_l2_name,
            router_uuid,
            hive_id,
            config_dir,
            state_dir,
            node_socket_dir,
            node_socket_path,
            shm_name,
            shm_prefix,
            is_gateway,
            hello_interval_ms,
            dead_interval_ms,
            heartbeat_interval_ms,
            heartbeat_stale_ms,
            wan_listen,
            wan_uplinks,
            wan_mtls_mode,
            tls_dir,
            hive_role,
            identity_frontdesk_node_name,
        })
    }
}

/// `wan.mtls`, fail-closed as `identity.sync.auth` is since the identity audit's F-01 (FINDINGS
/// A-178): an absent, empty or unknown value is `required`, an unknown one with a warning; only an
/// explicit `permissive` or `disabled` loosens it, `disabled` with a warning. A typo used to run
/// the WAN in plaintext, where a peer's hive is not authenticated.
fn wan_mtls_mode_from(raw: Option<&str>) -> &'static str {
    let Some(raw) = raw else {
        return "required";
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "required" | "" => "required",
        "permissive" => "permissive",
        "disabled" => {
            tracing::warn!(
                "wan.mtls=disabled: the WAN runs in plaintext, and a peer's hive is not \
                 authenticated. Only on a strictly isolated network."
            );
            "disabled"
        }
        other => {
            tracing::warn!(
                "wan.mtls='{other}' is unrecognized; treating it as required (fail-closed). Use \
                 required, permissive or (insecure) disabled."
            );
            "required"
        }
    }
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, hive_id)
    }
}

fn identity_path(state_dir: &Path, router_l2_name: &str) -> PathBuf {
    state_dir.join(router_l2_name).join("identity.yaml")
}

fn is_gateway_name(router_name: &str, gateway_name: &str) -> bool {
    let base = router_name.split('@').next().unwrap_or(router_name);
    let gateway_base = gateway_name.split('@').next().unwrap_or(gateway_name);
    base == gateway_base
}

fn build_identity_yaml(
    router_l2_name: &str,
    uuid: Uuid,
    shm_name: &str,
) -> Result<String, ConfigError> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let value = serde_yaml::to_string(&serde_yaml::Value::Mapping(
        [
            (
                serde_yaml::Value::String("layer1".to_string()),
                serde_yaml::Value::Mapping(
                    [(
                        serde_yaml::Value::String("uuid".to_string()),
                        serde_yaml::Value::String(uuid.to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                serde_yaml::Value::String("layer2".to_string()),
                serde_yaml::Value::Mapping(
                    [(
                        serde_yaml::Value::String("name".to_string()),
                        serde_yaml::Value::String(router_l2_name.to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                serde_yaml::Value::String("shm".to_string()),
                serde_yaml::Value::Mapping(
                    [(
                        serde_yaml::Value::String("name".to_string()),
                        serde_yaml::Value::String(shm_name.to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                serde_yaml::Value::String("created_at".to_string()),
                serde_yaml::Value::String(created_at.to_string()),
            ),
        ]
        .into_iter()
        .collect(),
    ))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    // FINDINGS A-178: as `auth_mode_required` in SY.identity (F-01).
    #[test]
    fn wan_mtls_is_fail_closed() {
        // Absent, empty, whitespace or a typo: required.
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("require"),
            Some("requird"),
            Some("true"),
            Some("enabled"),
            Some("off"),
        ] {
            assert_eq!(wan_mtls_mode_from(raw), "required", "{raw:?}");
        }
        assert_eq!(wan_mtls_mode_from(Some("  Required ")), "required");
        // Only the explicit values loosen it.
        assert_eq!(wan_mtls_mode_from(Some("permissive")), "permissive");
        assert_eq!(wan_mtls_mode_from(Some("PERMISSIVE")), "permissive");
        assert_eq!(wan_mtls_mode_from(Some("disabled")), "disabled");
        assert_eq!(wan_mtls_mode_from(Some(" Disabled ")), "disabled");
    }

    /// An old hive.yaml keeps its `ai` and `blob` keys (never removed), and the router still
    /// starts on it, whatever they hold: an `ai` no reader could use stopped it until 0.1.124.
    #[test]
    fn hive_yaml_ai_and_blob_never_stop_the_router() {
        let worker = r#"
hive_id: worker1
role: " Worker "
wan:
  gateway_name: RT.gateway
  uplinks:
    - address: "192.168.8.207:9000"
  mtls: required
government:
  identity_frontdesk: SY.frontdesk.gov@motherbee
blob:
  enabled: true
  path: /var/lib/fluxbee/blob
  sync:
    enabled: true
    tool: syncthing
    api_port: 8384
    data_dir: /var/lib/fluxbee/syncthing
ai:
  default_provider: anthropic
  providers:
    openai:
      model: gpt-5.5
"#;
        // `ai` names a provider it lists no model for: `HiveAiConfig::effective` refuses it.
        let settings = hive_settings_from(worker).expect("an old worker hive.yaml loads");
        assert_eq!(settings.hive_id, "worker1");
        assert_eq!(settings.hive_role, "worker");
        assert_eq!(settings.gateway_name, "RT.gateway");
        assert_eq!(settings.wan_listen, None);
        assert_eq!(settings.wan_uplinks, ["192.168.8.207:9000"]);
        assert_eq!(settings.wan_mtls_mode, "required");
        assert_eq!(
            settings.identity_frontdesk_node_name,
            "SY.frontdesk.gov@motherbee"
        );

        // Keys no reader could even type.
        for (ai, blob) in [
            ("{default_provider: gemini}", "{enabled: maybe}"),
            ("[openai, anthropic]", "42"),
            ("not a map", "{sync: [1, 2]}"),
        ] {
            let yaml = format!("hive_id: motherbee\nrole: motherbee\nai: {ai}\nblob: {blob}\n");
            let settings = hive_settings_from(&yaml).unwrap_or_else(|err| panic!("{yaml}: {err}"));
            assert_eq!(settings.hive_id, "motherbee");
            assert_eq!(settings.hive_role, "motherbee");
            assert_eq!(
                settings.identity_frontdesk_node_name,
                "SY.frontdesk.gov@motherbee"
            );
        }
    }
}
