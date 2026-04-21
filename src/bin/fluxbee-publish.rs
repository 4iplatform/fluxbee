use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use json_router::runtime_manifest::{
    load_runtime_manifest_from_paths, write_runtime_manifest_file_atomic, RuntimeManifest,
    RuntimeManifestEntry,
};
use json_router::runtime_package::{
    build_dry_run_plan, install_validated_package, package_type_label,
    update_runtime_manifest_with_package, validate_package, PackageMetadata, PackageType,
    ValidatedPackage, DIST_RUNTIME_MANIFEST_PATH, DIST_RUNTIME_ROOT_DIR,
};
use sha2::{Digest, Sha256};

type CliError = Box<dyn Error + Send + Sync>;
const DEFAULT_ADMIN_BASE_URL: &str = "http://127.0.0.1:8080";
const DEFAULT_MOTHER_HIVE_ID: &str = "motherbee";
const DEPLOY_SYNC_HINT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishCliArgs {
    package_dir: PathBuf,
    version_override: Option<String>,
    dry_run: bool,
    deploy_hive: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeManifestMeta {
    version: u64,
    hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeployResult {
    sync_hint_status: String,
    update_status: String,
    update_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpJsonResponse {
    status_code: u16,
    json: serde_json::Value,
}

fn trim_trailing_slash(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

fn admin_base_url() -> String {
    std::env::var("FLUXBEE_PUBLISH_BASE")
        .or_else(|_| std::env::var("BASE"))
        .unwrap_or_else(|_| DEFAULT_ADMIN_BASE_URL.to_string())
}

fn mother_hive_id() -> String {
    std::env::var("FLUXBEE_PUBLISH_MOTHER_HIVE_ID")
        .or_else(|_| std::env::var("MOTHER_HIVE_ID"))
        .unwrap_or_else(|_| DEFAULT_MOTHER_HIVE_ID.to_string())
}

fn curl_json(
    method: &str,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<HttpJsonResponse, String> {
    let mut cmd = Command::new("curl");
    cmd.arg("-sS")
        .arg("-X")
        .arg(method)
        .arg("-w")
        .arg("\n%{http_code}")
        .arg(url);
    if let Some(body_json) = body {
        cmd.arg("-H").arg("Content-Type: application/json");
        let payload = serde_json::to_string(body_json)
            .map_err(|err| format!("http request build failed for '{}': {}", url, err))?;
        cmd.arg("-d").arg(payload);
    }
    let out = cmd
        .output()
        .map_err(|err| format!("failed to execute curl for '{}': {}", url, err))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "curl failed for '{}' method={} exit={} stderr={}",
            url, method, out.status, stderr
        ));
    }
    let stdout = String::from_utf8(out.stdout)
        .map_err(|err| format!("invalid utf-8 response from '{}': {}", url, err))?;
    let (body_text, code_text) = stdout.rsplit_once('\n').ok_or_else(|| {
        format!(
            "invalid curl response framing for '{}': expected '<json>\\n<http_code>'",
            url
        )
    })?;
    let status_code = code_text
        .trim()
        .parse::<u16>()
        .map_err(|err| format!("invalid http status code from '{}': {}", url, err))?;
    let json: serde_json::Value = serde_json::from_str(body_text).map_err(|err| {
        format!(
            "invalid json response from '{}' (http={}): {} body={}",
            url,
            status_code,
            err,
            body_text.trim()
        )
    })?;
    Ok(HttpJsonResponse { status_code, json })
}

fn parse_runtime_manifest_meta_from_versions(
    json: &serde_json::Value,
) -> Result<RuntimeManifestMeta, String> {
    let runtimes = json
        .get("payload")
        .and_then(|v| v.get("hive"))
        .and_then(|v| v.get("runtimes"))
        .ok_or_else(|| "versions response missing payload.hive.runtimes".to_string())?;
    let version = runtimes
        .get("manifest_version")
        .and_then(|v| v.as_u64())
        .or_else(|| runtimes.get("version").and_then(|v| v.as_u64()))
        .ok_or_else(|| "versions response missing runtimes.manifest_version|version".to_string())?;
    let hash = runtimes
        .get("manifest_hash")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            runtimes
                .get("hash")
                .and_then(|v| v.as_str())
                .filter(|v| !v.trim().is_empty())
        })
        .ok_or_else(|| "versions response missing runtimes.manifest_hash|hash".to_string())?;
    Ok(RuntimeManifestMeta {
        version,
        hash: hash.to_string(),
    })
}

fn local_runtime_manifest_meta_from_file(path: &Path) -> Result<RuntimeManifestMeta, String> {
    let raw = fs::read(path).map_err(|err| {
        format!(
            "local manifest fallback failed: read '{}' failed: {}",
            path.display(),
            err
        )
    })?;
    let json: serde_json::Value = serde_json::from_slice(&raw).map_err(|err| {
        format!(
            "local manifest fallback failed: parse '{}' failed: {}",
            path.display(),
            err
        )
    })?;
    let version = json
        .get("version")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| json.get("version").and_then(|v| v.as_u64()))
        .ok_or_else(|| {
            format!(
                "local manifest fallback failed: missing version in '{}'",
                path.display()
            )
        })?;
    let mut hasher = Sha256::new();
    hasher.update(&raw);
    let hash = format!("{:x}", hasher.finalize());
    Ok(RuntimeManifestMeta { version, hash })
}

fn response_status_and_error_code(json: &serde_json::Value) -> (String, Option<String>) {
    let status = json
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("error")
        .to_string();
    let error_code = json
        .get("error_code")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .or_else(|| {
            json.get("payload")
                .and_then(|v| v.get("error_code"))
                .and_then(|v| v.as_str())
                .map(|v| v.to_string())
        });
    (status, error_code)
}

fn fetch_runtime_manifest_meta(
    base_url: &str,
    source_hive: &str,
) -> Result<RuntimeManifestMeta, String> {
    let url = format!(
        "{}/hives/{}/versions",
        trim_trailing_slash(base_url),
        source_hive
    );
    let response = curl_json("GET", &url, None)?;
    if response.status_code >= 400 {
        return Err(format!(
            "versions request failed http={} url='{}' body={}",
            response.status_code, url, response.json
        ));
    }
    match parse_runtime_manifest_meta_from_versions(&response.json) {
        Ok(meta) => Ok(meta),
        Err(parse_err) => {
            let fallback =
                local_runtime_manifest_meta_from_file(Path::new(DIST_RUNTIME_MANIFEST_PATH));
            fallback.map_err(|fallback_err| {
                format!(
                    "versions parse failed: {}; fallback also failed: {}",
                    parse_err, fallback_err
                )
            })
        }
    }
}

fn deploy_to_hive(
    base_url: &str,
    target_hive: &str,
    manifest_meta: &RuntimeManifestMeta,
) -> Result<DeployResult, String> {
    let base = trim_trailing_slash(base_url);
    let sync_hint_url = format!("{}/hives/{}/sync-hint", base, target_hive);
    let sync_hint_body = serde_json::json!({
        "channel":"dist",
        "folder_id":"fluxbee-dist",
        "wait_for_idle": true,
        "timeout_ms": DEPLOY_SYNC_HINT_TIMEOUT_MS
    });
    let sync_resp = curl_json("POST", &sync_hint_url, Some(&sync_hint_body))?;
    let (sync_hint_status, _) = response_status_and_error_code(&sync_resp.json);

    let update_url = format!("{}/hives/{}/update", base, target_hive);
    let update_body = serde_json::json!({
        "category":"runtime",
        "manifest_version": manifest_meta.version,
        "manifest_hash": manifest_meta.hash
    });
    let update_resp = curl_json("POST", &update_url, Some(&update_body))?;
    let (update_status, update_error_code) = response_status_and_error_code(&update_resp.json);

    Ok(DeployResult {
        sync_hint_status,
        update_status,
        update_error_code,
    })
}

fn usage(program: &str) -> String {
    format!(
        "\
Usage:
  {program} <package_dir> [--version <runtime_version>] [--dry-run] [--deploy <hive_id>]
  {program} --help

Options:
  --version <runtime_version>   Override package.json version for publish
  --dry-run                     Validate and print plan without installing
  --deploy <hive_id>            Trigger sync/update on target hive after publish
  --help                        Show this help
"
    )
}

fn parse_publish_cli_args(args: &[String]) -> Result<PublishCliArgs, String> {
    if args.is_empty() {
        return Err("missing arguments".to_string());
    }

    let mut package_dir: Option<PathBuf> = None;
    let mut version_override: Option<String> = None;
    let mut dry_run = false;
    let mut deploy_hive: Option<String> = None;

    let mut i = 1usize;
    while i < args.len() {
        let token = &args[i];
        match token.as_str() {
            "--help" | "-h" => {
                return Err("__help__".to_string());
            }
            "--dry-run" => {
                if dry_run {
                    return Err("duplicate flag: --dry-run".to_string());
                }
                dry_run = true;
                i += 1;
            }
            "--version" => {
                if version_override.is_some() {
                    return Err("duplicate flag: --version".to_string());
                }
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "missing value for --version".to_string())?;
                if value.starts_with('-') {
                    return Err("invalid value for --version".to_string());
                }
                version_override = Some(value.clone());
                i += 2;
            }
            "--deploy" => {
                if deploy_hive.is_some() {
                    return Err("duplicate flag: --deploy".to_string());
                }
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| "missing value for --deploy".to_string())?;
                if value.starts_with('-') {
                    return Err("invalid value for --deploy".to_string());
                }
                deploy_hive = Some(value.clone());
                i += 2;
            }
            _ if token.starts_with('-') => {
                return Err(format!("unknown option: {token}"));
            }
            _ => {
                if package_dir.is_some() {
                    return Err(format!("unexpected extra positional argument: {token}"));
                }
                package_dir = Some(PathBuf::from(token));
                i += 1;
            }
        }
    }

    let package_dir = package_dir.ok_or_else(|| "missing required <package_dir>".to_string())?;
    Ok(PublishCliArgs {
        package_dir,
        version_override,
        dry_run,
        deploy_hive,
    })
}

fn run(args: PublishCliArgs) -> Result<(), CliError> {
    println!("fluxbee-publish v{}", env!("CARGO_PKG_VERSION"));
    let validated = validate_package(&args.package_dir, args.version_override.as_deref())
        .map_err(|err| -> CliError { err.into() })?;

    println!("Package: {}", validated.metadata.name);
    println!("Version: {}", validated.effective_version);
    println!("Type: {}", package_type_label(validated.package_type));
    println!("Path: {}", validated.package_dir.display());
    if let Some(runtime_base) = validated.metadata.runtime_base.as_deref() {
        println!("Base: {runtime_base}");
    }
    println!("Validation: OK");

    if args.dry_run {
        let plan = build_dry_run_plan(&validated, args.deploy_hive.as_deref());
        println!("Dry-run: completed validation. No filesystem changes applied.");
        println!("Plan:");
        println!("  source_dir: {}", plan.source_dir.display());
        println!("  target_dir: {}", plan.target_dir.display());
        println!("  manifest_path: {}", plan.manifest_path.display());
        if let Some(runtime_base) = plan.runtime_base.as_deref() {
            println!("  runtime_base: {runtime_base}");
        }
        if let Some(deploy_target) = plan.deploy_target.as_deref() {
            println!("  deploy_target: {deploy_target}");
        }
        return Ok(());
    }
    let manifest_path = PathBuf::from(DIST_RUNTIME_MANIFEST_PATH);
    if !manifest_path.exists() {
        return Err(format!(
            "publish aborted: runtime manifest missing at '{}' (initialize dist first)",
            manifest_path.display()
        )
        .into());
    }
    let install =
        install_validated_package(&validated, Path::new(DIST_RUNTIME_ROOT_DIR), &manifest_path)
            .map_err(|err| -> CliError { err.into() })?;
    println!(
        "Install: OK (files={} bytes={})",
        install.copied_files, install.copied_bytes
    );
    println!("Installed path: {}", install.installed_path.display());
    println!(
        "Manifest: {} (version={})",
        install.manifest_path.display(),
        install.manifest_version
    );
    if let Some(target) = args.deploy_hive.as_deref() {
        let base_url = admin_base_url();
        let source_hive = mother_hive_id();
        let manifest_meta = fetch_runtime_manifest_meta(&base_url, &source_hive)
            .map_err(|err| -> CliError { err.into() })?;
        let deploy = deploy_to_hive(&base_url, target, &manifest_meta)
            .map_err(|err| -> CliError { err.into() })?;
        println!("Deploy: {}", target);
        println!(
            "  sync_hint_status={} update_status={} update_error_code={}",
            deploy.sync_hint_status,
            deploy.update_status,
            deploy.update_error_code.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn main() -> Result<(), CliError> {
    let args: Vec<String> = std::env::args().collect();
    let program = args
        .first()
        .map_or_else(|| "fluxbee-publish".to_string(), |v| v.clone());
    match parse_publish_cli_args(&args) {
        Ok(parsed) => run(parsed),
        Err(err) if err == "__help__" => {
            println!("{}", usage(&program));
            Ok(())
        }
        Err(err) => {
            eprintln!("Error: {err}");
            eprintln!();
            eprintln!("{}", usage(&program));
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write file");
    }

    fn make_executable(path: &Path) {
        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod");
    }

    fn write_package_json(dir: &Path, raw: &str) {
        write_file(&dir.join("package.json"), raw);
    }

    #[test]
    fn parse_publish_cli_args_accepts_minimal_positional_dir() {
        let args = vec!["fluxbee-publish".to_string(), ".".to_string()];
        let parsed = parse_publish_cli_args(&args).expect("expected valid args");
        assert_eq!(
            parsed,
            PublishCliArgs {
                package_dir: PathBuf::from("."),
                version_override: None,
                dry_run: false,
                deploy_hive: None,
            }
        );
    }

    #[test]
    fn parse_publish_cli_args_accepts_full_option_set() {
        let args = vec![
            "fluxbee-publish".to_string(),
            "./pkg".to_string(),
            "--version".to_string(),
            "1.2.3".to_string(),
            "--dry-run".to_string(),
            "--deploy".to_string(),
            "worker-220".to_string(),
        ];
        let parsed = parse_publish_cli_args(&args).expect("expected valid args");
        assert_eq!(parsed.package_dir, PathBuf::from("./pkg"));
        assert_eq!(parsed.version_override.as_deref(), Some("1.2.3"));
        assert!(parsed.dry_run);
        assert_eq!(parsed.deploy_hive.as_deref(), Some("worker-220"));
    }

    #[test]
    fn parse_publish_cli_args_requires_package_dir() {
        let args = vec!["fluxbee-publish".to_string(), "--dry-run".to_string()];
        let err = parse_publish_cli_args(&args).expect_err("expected error");
        assert!(
            err.contains("missing required <package_dir>"),
            "error={err}"
        );
    }

    #[test]
    fn parse_publish_cli_args_rejects_unknown_option() {
        let args = vec![
            "fluxbee-publish".to_string(),
            "./pkg".to_string(),
            "--unknown".to_string(),
        ];
        let err = parse_publish_cli_args(&args).expect_err("expected error");
        assert!(err.contains("unknown option"), "error={err}");
    }

    #[test]
    fn validate_package_accepts_full_runtime_with_executable_start_sh() {
        let dir = test_temp_dir("fluxbee-publish-full-ok");
        write_package_json(
            &dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);

        let out = validate_package(&dir, None).expect("validate should succeed");
        assert_eq!(out.metadata.name, "sy.frontdesk.gov");
        assert_eq!(package_type_label(out.package_type), "full_runtime");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_package_rejects_invalid_runtime_name() {
        let dir = test_temp_dir("fluxbee-publish-name-bad");
        write_package_json(
            &dir,
            r#"{
  "name": "AI.Frontdesk",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);

        let err = validate_package(&dir, None).expect_err("expected error");
        assert!(err.contains("naming policy"), "err={err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_package_rejects_invalid_semver() {
        let dir = test_temp_dir("fluxbee-publish-semver-bad");
        write_package_json(
            &dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "v1",
  "type": "full_runtime"
}"#,
        );
        let start_sh = dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);

        let err = validate_package(&dir, None).expect_err("expected error");
        assert!(err.contains("not valid semver"), "err={err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_package_rejects_missing_start_sh_for_full_runtime() {
        let dir = test_temp_dir("fluxbee-publish-full-missing-start");
        write_package_json(
            &dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let err = validate_package(&dir, None).expect_err("expected error");
        assert!(err.contains("missing entry point"), "err={err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_package_rejects_config_only_without_runtime_base() {
        let dir = test_temp_dir("fluxbee-publish-config-no-base");
        write_package_json(
            &dir,
            r#"{
  "name": "ai.billing",
  "version": "1.2.0",
  "type": "config_only"
}"#,
        );
        let err = validate_package(&dir, None).expect_err("expected error");
        assert!(err.contains("runtime_base is required"), "err={err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_package_rejects_workflow_without_flow_dir() {
        let dir = test_temp_dir("fluxbee-publish-workflow-no-flow");
        write_package_json(
            &dir,
            r#"{
  "name": "wf.onboarding.standard",
  "version": "1.0.0",
  "type": "workflow",
  "runtime_base": "wf.engine"
}"#,
        );
        let err = validate_package(&dir, None).expect_err("expected error");
        assert!(err.contains("missing required flow/"), "err={err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_package_accepts_workflow_with_flow_dir_and_base() {
        let dir = test_temp_dir("fluxbee-publish-workflow-ok");
        write_package_json(
            &dir,
            r#"{
  "name": "wf.onboarding.standard",
  "version": "1.0.0",
  "type": "workflow",
  "runtime_base": "wf.engine"
}"#,
        );
        fs::create_dir_all(dir.join("flow")).expect("create flow dir");
        write_file(&dir.join("flow/definition.json"), "{\"steps\":[]}");

        let out = validate_package(&dir, Some("1.0.1")).expect("validate should succeed");
        assert_eq!(out.effective_version, "1.0.1");
        assert_eq!(package_type_label(out.package_type), "workflow");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn build_dry_run_plan_uses_dist_runtime_target_path() {
        let validated = ValidatedPackage {
            package_dir: PathBuf::from("/tmp/pkg"),
            metadata: PackageMetadata {
                name: "sy.frontdesk.gov".to_string(),
                version: "1.0.0".to_string(),
                package_type: "full_runtime".to_string(),
                description: None,
                runtime_base: None,
                config_template: None,
                entry_point: None,
            },
            package_type: PackageType::FullRuntime,
            effective_version: "1.0.1".to_string(),
        };
        let plan = build_dry_run_plan(&validated, Some("worker-220"));
        assert_eq!(
            plan.target_dir,
            PathBuf::from("/var/lib/fluxbee/dist/runtimes/sy.frontdesk.gov/1.0.1")
        );
        assert_eq!(
            plan.manifest_path,
            PathBuf::from("/var/lib/fluxbee/dist/runtimes/manifest.json")
        );
        assert_eq!(plan.deploy_target.as_deref(), Some("worker-220"));
    }

    #[test]
    fn run_returns_ok_in_dry_run_for_valid_package() {
        let dir = test_temp_dir("fluxbee-publish-run-dry-ok");
        write_package_json(
            &dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);
        let out = run(PublishCliArgs {
            package_dir: dir.clone(),
            version_override: None,
            dry_run: true,
            deploy_hive: Some("worker-220".to_string()),
        });
        assert!(out.is_ok(), "expected dry-run success, got {out:?}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn run_returns_manifest_missing_without_dry_run() {
        let dir = test_temp_dir("fluxbee-publish-run-install-pending");
        write_package_json(
            &dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);
        let err = run(PublishCliArgs {
            package_dir: dir.clone(),
            version_override: None,
            dry_run: false,
            deploy_hive: None,
        })
        .expect_err("expected manifest missing error");
        assert!(
            err.to_string().contains("runtime manifest missing"),
            "error={err}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn install_validated_package_copies_files_sets_permissions_and_updates_manifest() {
        let package_dir = test_temp_dir("fluxbee-publish-install-ok-src");
        write_package_json(
            &package_dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = package_dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);
        write_file(
            &package_dir.join("config/default-config.json"),
            "{\"k\":\"v\"}",
        );

        let dist_root = test_temp_dir("fluxbee-publish-install-ok-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let validated = validate_package(&package_dir, None).expect("validate");
        let install =
            install_validated_package(&validated, &runtimes_root, &manifest_path).expect("install");
        assert_eq!(install.runtime_name, "sy.frontdesk.gov");
        assert_eq!(install.runtime_version, "1.0.0");
        assert!(install.manifest_version > 1710000000000);
        assert!(install.installed_path.exists());

        let installed_start = install.installed_path.join("bin/start.sh");
        assert!(installed_start.exists());
        let start_mode = fs::metadata(&installed_start)
            .expect("stat installed start")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(start_mode, 0o755);
        let installed_cfg = install.installed_path.join("config/default-config.json");
        let cfg_mode = fs::metadata(&installed_cfg)
            .expect("stat installed cfg")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(cfg_mode, 0o644);

        let loaded = load_runtime_manifest_from_paths(std::slice::from_ref(&manifest_path))
            .expect("load manifest")
            .expect("manifest exists");
        let runtimes = loaded.runtimes.as_object().expect("runtimes object");
        let entry_value = runtimes.get("sy.frontdesk.gov").expect("runtime entry");
        let entry: RuntimeManifestEntry =
            serde_json::from_value(entry_value.clone()).expect("entry deserialize");
        assert_eq!(entry.current.as_deref(), Some("1.0.0"));
        assert!(entry.available.iter().any(|v| v == "1.0.0"));
        assert_eq!(entry.package_type.as_deref(), Some("full_runtime"));
        assert_eq!(entry.runtime_base, None);

        let _ = fs::remove_dir_all(package_dir);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn install_validated_package_is_idempotent_for_same_contents() {
        let package_dir = test_temp_dir("fluxbee-publish-install-idempotent-src");
        write_package_json(
            &package_dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = package_dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);

        let dist_root = test_temp_dir("fluxbee-publish-install-idempotent-dist");
        let runtimes_root = dist_root.join("runtimes");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let validated = validate_package(&package_dir, None).expect("validate");
        let first = install_validated_package(&validated, &runtimes_root, &manifest_path)
            .expect("first install");
        let second = install_validated_package(&validated, &runtimes_root, &manifest_path)
            .expect("second install should be idempotent");
        assert_eq!(second.copied_files, 0);
        assert_eq!(second.copied_bytes, 0);
        assert_eq!(second.manifest_version, first.manifest_version);

        let _ = fs::remove_dir_all(package_dir);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn install_validated_package_rejects_existing_target_with_different_contents() {
        let package_dir = test_temp_dir("fluxbee-publish-install-conflict-src");
        write_package_json(
            &package_dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = package_dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho new\n");
        make_executable(&start_sh);

        let dist_root = test_temp_dir("fluxbee-publish-install-conflict-dist");
        let runtimes_root = dist_root.join("runtimes");
        let target_dir = runtimes_root.join("sy.frontdesk.gov/1.0.0");
        fs::create_dir_all(target_dir.join("bin")).expect("precreate target");
        write_package_json(
            &target_dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let existing_start = target_dir.join("bin/start.sh");
        write_file(&existing_start, "#!/usr/bin/env bash\necho old\n");
        make_executable(&existing_start);

        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let validated = validate_package(&package_dir, None).expect("validate");
        let err = install_validated_package(&validated, &runtimes_root, &manifest_path)
            .expect_err("expected existing target conflict");
        assert!(err.contains("different contents"), "err={err}");

        let _ = fs::remove_dir_all(package_dir);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn update_runtime_manifest_with_package_preserves_entry_extra_fields() {
        let dist_root = test_temp_dir("fluxbee-publish-manifest-extra");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "sy.frontdesk.gov": {
                    "available": ["0.9.0"],
                    "current": "0.9.0",
                    "type": "full_runtime",
                    "custom_flag": true
                }
            }),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let package_dir = test_temp_dir("fluxbee-publish-manifest-extra-src");
        write_package_json(
            &package_dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = package_dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);
        let validated = validate_package(&package_dir, None).expect("validate");

        let new_version =
            update_runtime_manifest_with_package(&manifest_path, &validated).expect("update");
        assert!(new_version > 1710000000000);
        let loaded = load_runtime_manifest_from_paths(std::slice::from_ref(&manifest_path))
            .expect("load manifest")
            .expect("manifest exists");
        let runtimes = loaded.runtimes.as_object().expect("runtimes object");
        let entry: RuntimeManifestEntry = serde_json::from_value(
            runtimes
                .get("sy.frontdesk.gov")
                .cloned()
                .expect("runtime entry"),
        )
        .expect("entry");
        assert!(entry.available.iter().any(|v| v == "0.9.0"));
        assert!(entry.available.iter().any(|v| v == "1.0.0"));
        assert_eq!(
            entry.extra.get("custom_flag"),
            Some(&serde_json::json!(true))
        );

        let _ = fs::remove_dir_all(package_dir);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn parse_runtime_manifest_meta_from_versions_extracts_version_and_hash() {
        let json = serde_json::json!({
            "status": "ok",
            "payload": {
                "hive": {
                    "runtimes": {
                        "manifest_version": 1711111111111u64,
                        "manifest_hash": "abc123",
                        "runtimes": {}
                    }
                }
            }
        });
        let meta = parse_runtime_manifest_meta_from_versions(&json).expect("meta");
        assert_eq!(meta.version, 1711111111111u64);
        assert_eq!(meta.hash, "abc123");
    }

    #[test]
    fn parse_runtime_manifest_meta_from_versions_rejects_missing_hash() {
        let json = serde_json::json!({
            "status": "ok",
            "payload": {
                "hive": {
                    "runtimes": {
                        "manifest_version": 1711111111111u64
                    }
                }
            }
        });
        let err = parse_runtime_manifest_meta_from_versions(&json).expect_err("expected error");
        assert!(err.contains("manifest_hash|hash"), "err={err}");
    }

    #[test]
    fn parse_runtime_manifest_meta_from_versions_accepts_legacy_version_hash_keys() {
        let json = serde_json::json!({
            "status": "ok",
            "payload": {
                "hive": {
                    "runtimes": {
                        "version": 1711111111111u64,
                        "hash": "legacyhash"
                    }
                }
            }
        });
        let meta = parse_runtime_manifest_meta_from_versions(&json).expect("meta");
        assert_eq!(meta.version, 1711111111111u64);
        assert_eq!(meta.hash, "legacyhash");
    }

    #[test]
    fn response_status_and_error_code_reads_top_level_or_payload_code() {
        let with_top = serde_json::json!({
            "status": "error",
            "error_code": "VERSION_MISMATCH"
        });
        let (status_top, code_top) = response_status_and_error_code(&with_top);
        assert_eq!(status_top, "error");
        assert_eq!(code_top.as_deref(), Some("VERSION_MISMATCH"));

        let with_payload = serde_json::json!({
            "status": "sync_pending",
            "payload": {"error_code": "RUNTIME_NOT_READY"}
        });
        let (status_payload, code_payload) = response_status_and_error_code(&with_payload);
        assert_eq!(status_payload, "sync_pending");
        assert_eq!(code_payload.as_deref(), Some("RUNTIME_NOT_READY"));
    }

    #[test]
    fn install_validated_package_with_version_override_updates_target_manifest_and_package_json() {
        let package_dir = test_temp_dir("fluxbee-publish-install-override-src");
        write_package_json(
            &package_dir,
            r#"{
  "name": "sy.frontdesk.gov",
  "version": "1.0.0",
  "type": "full_runtime"
}"#,
        );
        let start_sh = package_dir.join("bin/start.sh");
        write_file(&start_sh, "#!/usr/bin/env bash\necho ok\n");
        make_executable(&start_sh);

        let dist_root = test_temp_dir("fluxbee-publish-install-override-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let validated = validate_package(&package_dir, Some("2.4.1")).expect("validate");
        let install =
            install_validated_package(&validated, &runtimes_root, &manifest_path).expect("install");
        assert!(install
            .installed_path
            .ends_with(Path::new("sy.frontdesk.gov/2.4.1")));

        let loaded = load_runtime_manifest_from_paths(std::slice::from_ref(&manifest_path))
            .expect("load manifest")
            .expect("manifest exists");
        let runtimes = loaded.runtimes.as_object().expect("runtimes object");
        let entry: RuntimeManifestEntry = serde_json::from_value(
            runtimes
                .get("sy.frontdesk.gov")
                .cloned()
                .expect("runtime entry"),
        )
        .expect("entry");
        assert_eq!(entry.current.as_deref(), Some("2.4.1"));
        assert!(entry.available.iter().any(|v| v == "2.4.1"));

        let installed_package_json =
            fs::read_to_string(install.installed_path.join("package.json"))
                .expect("read installed package.json");
        let installed_meta: serde_json::Value =
            serde_json::from_str(&installed_package_json).expect("parse installed package.json");
        assert_eq!(
            installed_meta.get("version"),
            Some(&serde_json::json!("2.4.1"))
        );

        let source_package_json =
            fs::read_to_string(package_dir.join("package.json")).expect("read source package.json");
        let source_meta: serde_json::Value =
            serde_json::from_str(&source_package_json).expect("parse source package.json");
        assert_eq!(
            source_meta.get("version"),
            Some(&serde_json::json!("1.0.0"))
        );

        let _ = fs::remove_dir_all(package_dir);
        let _ = fs::remove_dir_all(dist_root);
    }
}
