use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const RUNTIME_MANIFEST_SCHEMA_VERSION_MIN: u64 = 1;
pub const RUNTIME_MANIFEST_SCHEMA_VERSION_MAX: u64 = 2;
pub const RUNTIME_MANIFEST_WRITE_V2_GATE_ENV: &str = "FLUXBEE_RUNTIME_MANIFEST_WRITE_V2";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RuntimeManifest {
    #[serde(default = "default_runtime_manifest_schema_version")]
    pub schema_version: u64,
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub runtimes: serde_json::Value,
    #[serde(default)]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RuntimeManifestEntry {
    #[serde(default)]
    pub available: Vec<String>,
    #[serde(default)]
    pub current: Option<String>,
    #[serde(rename = "type", default)]
    pub package_type: Option<String>,
    #[serde(default)]
    pub runtime_base: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn default_runtime_manifest_schema_version() -> u64 {
    RUNTIME_MANIFEST_SCHEMA_VERSION_MIN
}

pub fn runtime_manifest_schema_supported(schema_version: u64) -> bool {
    schema_version >= RUNTIME_MANIFEST_SCHEMA_VERSION_MIN
        && schema_version <= RUNTIME_MANIFEST_SCHEMA_VERSION_MAX
}

pub fn parse_runtime_manifest_file(path: &Path, data: &str) -> Result<RuntimeManifest, String> {
    let manifest: RuntimeManifest = serde_json::from_str(data).map_err(|err| {
        format!(
            "runtime manifest parse error path='{}': {}",
            path.display(),
            err
        )
    })?;

    if !runtime_manifest_schema_supported(manifest.schema_version) {
        return Err(format!(
            "runtime manifest invalid: unsupported schema_version={} (supported={}..={}) path='{}'",
            manifest.schema_version,
            RUNTIME_MANIFEST_SCHEMA_VERSION_MIN,
            RUNTIME_MANIFEST_SCHEMA_VERSION_MAX,
            path.display()
        ));
    }
    if manifest.version == 0 {
        return Err(format!(
            "runtime manifest invalid: version must be > 0 path='{}'",
            path.display()
        ));
    }

    Ok(manifest)
}

pub fn load_runtime_manifest_from_paths(
    paths: &[PathBuf],
) -> Result<Option<RuntimeManifest>, String> {
    let mut errors: Vec<String> = Vec::new();
    for path in paths {
        let data = match fs::read_to_string(path) {
            Ok(data) => data,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => {
                errors.push(format!(
                    "runtime manifest read error path='{}': {}",
                    path.display(),
                    err
                ));
                continue;
            }
        };
        match parse_runtime_manifest_file(path, &data) {
            Ok(manifest) => return Ok(Some(manifest)),
            Err(err) => {
                errors.push(err);
                continue;
            }
        }
    }
    if errors.is_empty() {
        Ok(None)
    } else {
        Err(errors.join("; "))
    }
}

pub fn write_runtime_manifest_file_atomic(
    path: &Path,
    manifest: &RuntimeManifest,
    write_v2_gate_enabled: bool,
) -> Result<(), String> {
    let manifest = runtime_manifest_for_write(manifest, write_v2_gate_enabled);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "runtime manifest write error: create parent '{}' failed: {}",
                parent.display(),
                err
            )
        })?;
    }
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("runtime manifest serialize error: {}", err))?;
    fs::write(&tmp, &bytes).map_err(|err| {
        format!(
            "runtime manifest write error: temp file '{}' failed: {}",
            tmp.display(),
            err
        )
    })?;
    if let Err(err) = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644)) {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "runtime manifest write error: set permissions '{}' failed: {}",
            tmp.display(),
            err
        ));
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "runtime manifest write error: rename '{}' -> '{}' failed: {}",
            tmp.display(),
            path.display(),
            err
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).map_err(|err| {
        format!(
            "runtime manifest write error: set permissions '{}' failed: {}",
            path.display(),
            err
        )
    })?;
    Ok(())
}

pub fn runtime_manifest_write_v2_gate_enabled(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn runtime_manifest_write_v2_gate_enabled_from_env() -> bool {
    let raw = std::env::var(RUNTIME_MANIFEST_WRITE_V2_GATE_ENV).ok();
    runtime_manifest_write_v2_gate_enabled(raw.as_deref())
}

pub fn runtime_manifest_write_schema(write_v2_gate_enabled: bool) -> u64 {
    if write_v2_gate_enabled {
        RUNTIME_MANIFEST_SCHEMA_VERSION_MAX
    } else {
        RUNTIME_MANIFEST_SCHEMA_VERSION_MIN
    }
}

pub fn runtime_manifest_for_write(
    manifest: &RuntimeManifest,
    write_v2_gate_enabled: bool,
) -> RuntimeManifest {
    let mut out = manifest.clone();
    out.schema_version = runtime_manifest_write_schema(write_v2_gate_enabled);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_load_runtime_manifest_roundtrip() {
        let dir = std::env::temp_dir().join(format!("runtime-manifest-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710099999999,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.test": {"available": ["1.0.0"], "current":"1.0.0"}
            }),
            hash: None,
        };

        write_runtime_manifest_file_atomic(&path, &manifest, true).expect("write manifest");
        let loaded = load_runtime_manifest_from_paths(std::slice::from_ref(&path))
            .expect("load manifest")
            .expect("manifest should exist");

        assert_eq!(loaded, manifest);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_manifest_for_write_forces_schema_v1_without_gate() {
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710099999999,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        let out = runtime_manifest_for_write(&manifest, false);
        assert_eq!(out.schema_version, 1);
    }

    #[test]
    fn runtime_manifest_for_write_forces_schema_v2_with_gate() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710099999999,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        let out = runtime_manifest_for_write(&manifest, true);
        assert_eq!(out.schema_version, 2);
    }

    #[test]
    fn runtime_manifest_write_v2_gate_enabled_parses_truthy_and_falsy() {
        assert!(!runtime_manifest_write_v2_gate_enabled(None));
        assert!(!runtime_manifest_write_v2_gate_enabled(Some("0")));
        assert!(!runtime_manifest_write_v2_gate_enabled(Some("false")));
        assert!(runtime_manifest_write_v2_gate_enabled(Some("1")));
        assert!(runtime_manifest_write_v2_gate_enabled(Some("true")));
        assert!(runtime_manifest_write_v2_gate_enabled(Some("YES")));
        assert!(runtime_manifest_write_v2_gate_enabled(Some("on")));
    }
}
