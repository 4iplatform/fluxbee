use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use regex::Regex;
use serde::Deserialize;
use uuid::Uuid;

use crate::runtime_manifest::{
    load_runtime_manifest_from_paths, runtime_manifest_write_v2_gate_enabled_from_env,
    write_runtime_manifest_file_atomic, RuntimeManifestEntry,
};

pub const DIST_RUNTIME_ROOT_DIR: &str = "/var/lib/fluxbee/dist/runtimes";
pub const DIST_RUNTIME_MANIFEST_PATH: &str = "/var/lib/fluxbee/dist/runtimes/manifest.json";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub package_type: String,
    #[allow(dead_code)]
    pub description: Option<String>,
    pub runtime_base: Option<String>,
    pub config_template: Option<String>,
    pub entry_point: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageType {
    FullRuntime,
    ConfigOnly,
    Workflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPackage {
    pub package_dir: PathBuf,
    pub metadata: PackageMetadata,
    pub package_type: PackageType,
    pub effective_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunPlan {
    pub source_dir: PathBuf,
    pub target_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub runtime_name: String,
    pub runtime_version: String,
    pub package_type: String,
    pub runtime_base: Option<String>,
    pub deploy_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    pub runtime_name: String,
    pub runtime_version: String,
    pub package_type: String,
    pub installed_path: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_version: u64,
    pub copied_files: usize,
    pub copied_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CopyStats {
    files: usize,
    bytes: u64,
}

fn runtime_name_regex() -> Regex {
    Regex::new(r"^[a-z][a-z0-9]*(\.[a-z0-9]+)*$").expect("valid runtime name regex")
}

fn semver_regex() -> Regex {
    Regex::new(
        r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$",
    )
    .expect("valid semver regex")
}

fn parse_package_type(raw: &str) -> Result<PackageType, String> {
    match raw {
        "full_runtime" => Ok(PackageType::FullRuntime),
        "config_only" => Ok(PackageType::ConfigOnly),
        "workflow" => Ok(PackageType::Workflow),
        _ => Err(format!(
            "package.json invalid: unknown type='{raw}' (expected: full_runtime|config_only|workflow)"
        )),
    }
}

fn is_relative_package_path(path: &Path) -> bool {
    if path.is_absolute() {
        return false;
    }
    !path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

fn validate_runtime_name(field: &str, value: &str) -> Result<(), String> {
    if runtime_name_regex().is_match(value) {
        return Ok(());
    }
    Err(format!(
        "package.json invalid: {field}='{value}' does not match runtime naming policy (lowercase + dots)"
    ))
}

fn validate_semver(field: &str, value: &str) -> Result<(), String> {
    if semver_regex().is_match(value) {
        return Ok(());
    }
    Err(format!(
        "package.json invalid: {field}='{value}' is not valid semver"
    ))
}

fn assert_relative_path_exists_if_set(
    package_dir: &Path,
    value: Option<&str>,
    field: &str,
) -> Result<(), String> {
    let Some(raw) = value else {
        return Ok(());
    };
    let candidate = PathBuf::from(raw);
    if !is_relative_package_path(&candidate) {
        return Err(format!(
            "package.json invalid: {field} must be a relative package path (got '{raw}')"
        ));
    }
    let full_path = package_dir.join(&candidate);
    if !full_path.exists() {
        return Err(format!(
            "package.json invalid: {field} path not found '{}'",
            full_path.display()
        ));
    }
    Ok(())
}

fn load_package_metadata(package_dir: &Path) -> Result<PackageMetadata, String> {
    if !package_dir.exists() {
        return Err(format!(
            "package directory not found '{}'",
            package_dir.display()
        ));
    }
    if !package_dir.is_dir() {
        return Err(format!(
            "package path is not a directory '{}'",
            package_dir.display()
        ));
    }

    let package_json_path = package_dir.join("package.json");
    let raw = fs::read_to_string(&package_json_path).map_err(|err| {
        format!(
            "failed to read package.json '{}': {}",
            package_json_path.display(),
            err
        )
    })?;
    serde_json::from_str::<PackageMetadata>(&raw).map_err(|err| {
        format!(
            "failed to parse package.json '{}': {}",
            package_json_path.display(),
            err
        )
    })
}

pub fn validate_package(
    package_dir: &Path,
    version_override: Option<&str>,
) -> Result<ValidatedPackage, String> {
    let metadata = load_package_metadata(package_dir)?;
    validate_runtime_name("name", &metadata.name)?;
    validate_semver("version", &metadata.version)?;

    if let Some(raw) = version_override {
        validate_semver("version override", raw)?;
    }
    let effective_version = version_override
        .map(|v| v.to_string())
        .unwrap_or_else(|| metadata.version.clone());

    let package_type = parse_package_type(&metadata.package_type)?;

    match package_type {
        PackageType::FullRuntime => {
            if metadata
                .runtime_base
                .as_deref()
                .is_some_and(|v| !v.is_empty())
            {
                return Err(
                    "package.json invalid: runtime_base must be null/absent for full_runtime"
                        .to_string(),
                );
            }
            let entry_rel = metadata.entry_point.as_deref().unwrap_or("bin/start.sh");
            let entry_path = PathBuf::from(entry_rel);
            if !is_relative_package_path(&entry_path) {
                return Err(format!(
                    "package.json invalid: entry_point must be a relative package path (got '{entry_rel}')"
                ));
            }
            let start_sh = package_dir.join(entry_path);
            if !start_sh.exists() {
                return Err(format!(
                    "package validation failed: missing entry point '{}'",
                    start_sh.display()
                ));
            }
            if !start_sh.is_file() {
                return Err(format!(
                    "package validation failed: entry point is not a file '{}'",
                    start_sh.display()
                ));
            }
            let mode = fs::metadata(&start_sh)
                .map_err(|err| {
                    format!(
                        "package validation failed: stat entry point '{}': {}",
                        start_sh.display(),
                        err
                    )
                })?
                .permissions()
                .mode();
            if mode & 0o111 == 0 {
                return Err(format!(
                    "package validation failed: entry point is not executable '{}'",
                    start_sh.display()
                ));
            }
        }
        PackageType::ConfigOnly => {
            let runtime_base = metadata.runtime_base.as_deref().unwrap_or("").trim();
            if runtime_base.is_empty() {
                return Err(
                    "package.json invalid: runtime_base is required for config_only".to_string(),
                );
            }
            validate_runtime_name("runtime_base", runtime_base)?;
            assert_relative_path_exists_if_set(
                package_dir,
                metadata.config_template.as_deref(),
                "config_template",
            )?;
        }
        PackageType::Workflow => {
            let runtime_base = metadata.runtime_base.as_deref().unwrap_or("").trim();
            if runtime_base.is_empty() {
                return Err(
                    "package.json invalid: runtime_base is required for workflow".to_string(),
                );
            }
            validate_runtime_name("runtime_base", runtime_base)?;
            let flow_dir = package_dir.join("flow");
            if !flow_dir.exists() || !flow_dir.is_dir() {
                return Err(format!(
                    "package validation failed: missing required flow/ directory '{}'",
                    flow_dir.display()
                ));
            }
            assert_relative_path_exists_if_set(
                package_dir,
                metadata.config_template.as_deref(),
                "config_template",
            )?;
        }
    }

    Ok(ValidatedPackage {
        package_dir: package_dir.to_path_buf(),
        metadata,
        package_type,
        effective_version,
    })
}

pub fn package_type_label(package_type: PackageType) -> &'static str {
    match package_type {
        PackageType::FullRuntime => "full_runtime",
        PackageType::ConfigOnly => "config_only",
        PackageType::Workflow => "workflow",
    }
}

pub fn build_dry_run_plan(validated: &ValidatedPackage, deploy_target: Option<&str>) -> DryRunPlan {
    DryRunPlan {
        source_dir: validated.package_dir.clone(),
        target_dir: PathBuf::from(DIST_RUNTIME_ROOT_DIR)
            .join(&validated.metadata.name)
            .join(&validated.effective_version),
        manifest_path: PathBuf::from(DIST_RUNTIME_MANIFEST_PATH),
        runtime_name: validated.metadata.name.clone(),
        runtime_version: validated.effective_version.clone(),
        package_type: package_type_label(validated.package_type).to_string(),
        runtime_base: validated.metadata.runtime_base.clone(),
        deploy_target: deploy_target.map(|v| v.to_string()),
    }
}

fn first_component_is_bin(rel_path: &Path) -> bool {
    rel_path
        .components()
        .next()
        .is_some_and(|c| c.as_os_str() == "bin")
}

fn copy_tree_with_permissions(
    source_root: &Path,
    current_source: &Path,
    current_target: &Path,
    stats: &mut CopyStats,
) -> Result<(), String> {
    fs::create_dir_all(current_target).map_err(|err| {
        format!(
            "install failed: create directory '{}' failed: {}",
            current_target.display(),
            err
        )
    })?;
    fs::set_permissions(current_target, fs::Permissions::from_mode(0o755)).map_err(|err| {
        format!(
            "install failed: set directory permissions '{}' failed: {}",
            current_target.display(),
            err
        )
    })?;

    for entry_res in fs::read_dir(current_source).map_err(|err| {
        format!(
            "install failed: read directory '{}' failed: {}",
            current_source.display(),
            err
        )
    })? {
        let entry = entry_res.map_err(|err| {
            format!(
                "install failed: read entry in '{}' failed: {}",
                current_source.display(),
                err
            )
        })?;
        let source_path = entry.path();
        let target_path = current_target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|err| {
            format!(
                "install failed: read file type '{}' failed: {}",
                source_path.display(),
                err
            )
        })?;

        if file_type.is_symlink() {
            return Err(format!(
                "install failed: symlink is not allowed in package '{}'",
                source_path.display()
            ));
        }
        if file_type.is_dir() {
            copy_tree_with_permissions(source_root, &source_path, &target_path, stats)?;
            continue;
        }
        if file_type.is_file() {
            fs::copy(&source_path, &target_path).map_err(|err| {
                format!(
                    "install failed: copy '{}' -> '{}' failed: {}",
                    source_path.display(),
                    target_path.display(),
                    err
                )
            })?;
            let metadata = fs::metadata(&target_path).map_err(|err| {
                format!(
                    "install failed: stat copied file '{}' failed: {}",
                    target_path.display(),
                    err
                )
            })?;
            let rel = source_path.strip_prefix(source_root).map_err(|err| {
                format!(
                    "install failed: internal path error for '{}' against '{}': {}",
                    source_path.display(),
                    source_root.display(),
                    err
                )
            })?;
            let mode = if first_component_is_bin(rel) {
                0o755
            } else {
                0o644
            };
            fs::set_permissions(&target_path, fs::Permissions::from_mode(mode)).map_err(|err| {
                format!(
                    "install failed: set file permissions '{}' failed: {}",
                    target_path.display(),
                    err
                )
            })?;
            stats.files += 1;
            stats.bytes += metadata.len();
            continue;
        }
        return Err(format!(
            "install failed: unsupported filesystem entry '{}'",
            source_path.display()
        ));
    }

    Ok(())
}

fn install_package_files_atomic(source_dir: &Path, target_dir: &Path) -> Result<CopyStats, String> {
    if target_dir.exists() {
        return Err(format!(
            "install failed: target runtime version already exists '{}'",
            target_dir.display()
        ));
    }
    let parent = target_dir.parent().ok_or_else(|| {
        format!(
            "install failed: target directory has no parent '{}'",
            target_dir.display()
        )
    })?;
    fs::create_dir_all(parent).map_err(|err| {
        format!(
            "install failed: create target parent '{}' failed: {}",
            parent.display(),
            err
        )
    })?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).map_err(|err| {
        format!(
            "install failed: set target parent permissions '{}' failed: {}",
            parent.display(),
            err
        )
    })?;

    let staging_dir = parent.join(format!(
        ".publish-{}-{}-staging",
        target_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "runtime".to_string()),
        Uuid::new_v4().simple()
    ));
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).map_err(|err| {
            format!(
                "install failed: cleanup stale staging '{}' failed: {}",
                staging_dir.display(),
                err
            )
        })?;
    }

    let mut stats = CopyStats::default();
    if let Err(err) = copy_tree_with_permissions(source_dir, source_dir, &staging_dir, &mut stats) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(err);
    }
    if let Err(err) = fs::rename(&staging_dir, target_dir) {
        let _ = fs::remove_dir_all(&staging_dir);
        return Err(format!(
            "install failed: promote staging '{}' -> '{}' failed: {}",
            staging_dir.display(),
            target_dir.display(),
            err
        ));
    }
    Ok(stats)
}

fn apply_installed_package_json_version(target_dir: &Path, version: &str) -> Result<(), String> {
    let package_json_path = target_dir.join("package.json");
    let raw = fs::read_to_string(&package_json_path).map_err(|err| {
        format!(
            "install failed: read installed package.json '{}' failed: {}",
            package_json_path.display(),
            err
        )
    })?;
    let mut value: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        format!(
            "install failed: parse installed package.json '{}' failed: {}",
            package_json_path.display(),
            err
        )
    })?;
    let obj = value.as_object_mut().ok_or_else(|| {
        format!(
            "install failed: installed package.json root is not an object '{}'",
            package_json_path.display()
        )
    })?;
    obj.insert("version".to_string(), serde_json::json!(version));
    let pretty = serde_json::to_vec_pretty(&value).map_err(|err| {
        format!(
            "install failed: serialize installed package.json '{}' failed: {}",
            package_json_path.display(),
            err
        )
    })?;
    fs::write(&package_json_path, pretty).map_err(|err| {
        format!(
            "install failed: write installed package.json '{}' failed: {}",
            package_json_path.display(),
            err
        )
    })?;
    fs::set_permissions(&package_json_path, fs::Permissions::from_mode(0o644)).map_err(|err| {
        format!(
            "install failed: set installed package.json permissions '{}' failed: {}",
            package_json_path.display(),
            err
        )
    })?;
    Ok(())
}

fn runtime_entry_from_value(value: &serde_json::Value) -> Result<RuntimeManifestEntry, String> {
    serde_json::from_value(value.clone())
        .map_err(|err| format!("manifest invalid runtime entry structure: {err}"))
}

fn next_manifest_version_ms(now_ms: u64, current_manifest_version: u64) -> u64 {
    if now_ms > current_manifest_version {
        now_ms
    } else {
        current_manifest_version + 1
    }
}

pub fn update_runtime_manifest_with_package(
    manifest_path: &Path,
    validated: &ValidatedPackage,
) -> Result<u64, String> {
    let maybe_manifest =
        load_runtime_manifest_from_paths(&[manifest_path.to_path_buf()]).map_err(|err| {
            format!(
                "manifest update failed: unable to load '{}': {}",
                manifest_path.display(),
                err
            )
        })?;
    let mut manifest = maybe_manifest.ok_or_else(|| {
        format!(
            "manifest update failed: runtime manifest missing '{}'",
            manifest_path.display()
        )
    })?;
    let runtime_map = manifest.runtimes.as_object_mut().ok_or_else(|| {
        format!(
            "manifest update failed: runtimes field is not an object in '{}'",
            manifest_path.display()
        )
    })?;

    let existing = runtime_map.get(&validated.metadata.name).cloned();
    let mut entry = match existing {
        Some(value) => runtime_entry_from_value(&value)?,
        None => RuntimeManifestEntry::default(),
    };
    if !entry
        .available
        .iter()
        .any(|v| v == &validated.effective_version)
    {
        entry.available.push(validated.effective_version.clone());
    }
    entry.current = Some(validated.effective_version.clone());
    entry.package_type = Some(package_type_label(validated.package_type).to_string());
    entry.runtime_base = validated.metadata.runtime_base.clone();

    runtime_map.insert(
        validated.metadata.name.clone(),
        serde_json::to_value(entry).map_err(|err| {
            format!(
                "manifest update failed: serialize runtime entry '{}' failed: {}",
                validated.metadata.name, err
            )
        })?,
    );

    let now_ms_i64 = Utc::now().timestamp_millis();
    let now_ms = u64::try_from(now_ms_i64)
        .map_err(|_| format!("manifest update failed: invalid clock value {}", now_ms_i64))?;
    manifest.version = next_manifest_version_ms(now_ms, manifest.version);
    manifest.updated_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    manifest.hash = None;

    let write_v2_gate_enabled = runtime_manifest_write_v2_gate_enabled_from_env();
    write_runtime_manifest_file_atomic(manifest_path, &manifest, write_v2_gate_enabled).map_err(
        |err| {
            format!(
                "manifest update failed: write '{}' failed: {}",
                manifest_path.display(),
                err
            )
        },
    )?;
    Ok(manifest.version)
}

pub fn install_validated_package(
    validated: &ValidatedPackage,
    dist_runtime_root: &Path,
    manifest_path: &Path,
) -> Result<InstallResult, String> {
    let target_dir = dist_runtime_root
        .join(&validated.metadata.name)
        .join(&validated.effective_version);
    let copy_stats = install_package_files_atomic(&validated.package_dir, &target_dir)?;
    if let Err(err) =
        apply_installed_package_json_version(&target_dir, &validated.effective_version)
    {
        let _ = fs::remove_dir_all(&target_dir);
        return Err(err);
    }
    let manifest_version = match update_runtime_manifest_with_package(manifest_path, validated) {
        Ok(version) => version,
        Err(err) => {
            let rollback = fs::remove_dir_all(&target_dir);
            if let Err(rollback_err) = rollback {
                return Err(format!(
                    "{}; rollback failed for '{}': {}",
                    err,
                    target_dir.display(),
                    rollback_err
                ));
            }
            return Err(err);
        }
    };
    Ok(InstallResult {
        runtime_name: validated.metadata.name.clone(),
        runtime_version: validated.effective_version.clone(),
        package_type: package_type_label(validated.package_type).to_string(),
        installed_path: target_dir,
        manifest_path: manifest_path.to_path_buf(),
        manifest_version,
        copied_files: copy_stats.files,
        copied_bytes: copy_stats.bytes,
    })
}
