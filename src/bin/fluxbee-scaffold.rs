use std::fs;
use std::path::{Path, PathBuf};

type CliError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PackageType {
    FullRuntime,
    ConfigOnly,
    Workflow,
}

impl PackageType {
    fn as_str(&self) -> &'static str {
        match self {
            PackageType::FullRuntime => "full_runtime",
            PackageType::ConfigOnly => "config_only",
            PackageType::Workflow => "workflow",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            PackageType::FullRuntime => "full_runtime  (custom binary + assets)",
            PackageType::ConfigOnly => "config_only   (config + assets, uses base runtime)",
            PackageType::Workflow => "workflow      (flow definition, uses base runtime)",
        }
    }
}

#[derive(Debug, Clone)]
struct ScaffoldArgs {
    name: String,
    version: String,
    package_type: PackageType,
    runtime_base: Option<String>,
    output_dir: PathBuf,
}

fn usage(program: &str) -> String {
    format!(
        "\
Usage:
  {program} --name <runtime_name> --type <package_type> [options] [output_dir]
  {program} --help

Required:
  --name <runtime_name>   Runtime name (lowercase, dots as separators, e.g. ai.soporte.billing)
  --type <package_type>   One of: full_runtime, config_only, workflow

Options:
  --version <semver>      Initial version (default: 0.1.0)
  --base <runtime_name>   Base runtime (required for config_only and workflow)
  --help                  Show this help

Arguments:
  output_dir              Where to create the package directory (default: current dir)
                          The package directory <runtime_name> is created inside output_dir.

Examples:
  {program} --name ai.soporte.billing --type config_only --base ai.generic
  {program} --name sy.frontdesk.gov --type full_runtime --version 1.0.0
  {program} --name wf.onboarding.standard --type workflow --base wf.engine ./packages
"
    )
}

fn parse_args(args: &[String]) -> Result<ScaffoldArgs, String> {
    if args.is_empty() {
        return Err("missing arguments".to_string());
    }

    let mut name: Option<String> = None;
    let mut version = "0.1.0".to_string();
    let mut package_type: Option<PackageType> = None;
    let mut runtime_base: Option<String> = None;
    let mut output_dir: Option<PathBuf> = None;

    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Err("__help__".to_string()),
            "--name" => {
                let v = args.get(i + 1).ok_or("missing value for --name")?;
                name = Some(v.clone());
                i += 2;
            }
            "--version" => {
                let v = args.get(i + 1).ok_or("missing value for --version")?;
                version = v.clone();
                i += 2;
            }
            "--type" => {
                let v = args.get(i + 1).ok_or("missing value for --type")?;
                package_type = Some(match v.as_str() {
                    "full_runtime" => PackageType::FullRuntime,
                    "config_only" => PackageType::ConfigOnly,
                    "workflow" => PackageType::Workflow,
                    other => return Err(format!("unknown package type: '{other}' (valid: full_runtime, config_only, workflow)")),
                });
                i += 2;
            }
            "--base" => {
                let v = args.get(i + 1).ok_or("missing value for --base")?;
                runtime_base = Some(v.clone());
                i += 2;
            }
            t if t.starts_with('-') => return Err(format!("unknown option: {t}")),
            _ => {
                if output_dir.is_some() {
                    return Err(format!("unexpected extra argument: {}", args[i]));
                }
                output_dir = Some(PathBuf::from(&args[i]));
                i += 1;
            }
        }
    }

    let name = name.ok_or("missing required --name")?;
    let package_type = package_type.ok_or("missing required --type")?;

    validate_runtime_name(&name)?;
    validate_semver(&version)?;

    if matches!(package_type, PackageType::ConfigOnly | PackageType::Workflow)
        && runtime_base.is_none()
    {
        return Err(format!(
            "--base is required for package type '{}'",
            package_type.as_str()
        ));
    }

    Ok(ScaffoldArgs {
        name,
        version,
        package_type,
        runtime_base,
        output_dir: output_dir.unwrap_or_else(|| PathBuf::from(".")),
    })
}

fn validate_runtime_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("runtime name must not be empty".to_string());
    }
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '.' && ch != '-' {
            return Err(format!(
                "runtime name '{name}' violates naming policy: only lowercase letters, digits, dots, and hyphens are allowed"
            ));
        }
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err(format!(
            "runtime name '{name}' must not start or end with a dot"
        ));
    }
    Ok(())
}

fn validate_semver(version: &str) -> Result<(), String> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
        return Err(format!(
            "version '{version}' is not valid semver (expected MAJOR.MINOR.PATCH)"
        ));
    }
    Ok(())
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory '{}': {e}", parent.display()))?;
    }
    fs::write(path, content)
        .map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
    Ok(())
}

fn scaffold_full_runtime(pkg_dir: &Path, _args: &ScaffoldArgs) -> Result<Vec<PathBuf>, String> {
    let mut created = Vec::new();

    let start_sh = pkg_dir.join("bin/start.sh");
    write_file(
        &start_sh,
        "#!/usr/bin/env bash\n# Entry point — replace with your compiled binary\nexec \"$(dirname \"$0\")/node\" \"$@\"\n",
    )?;
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&start_sh)
        .map_err(|e| format!("stat bin/start.sh: {e}"))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&start_sh, perms)
        .map_err(|e| format!("chmod bin/start.sh: {e}"))?;
    created.push(start_sh);

    let prompt = pkg_dir.join("assets/prompts/system.txt");
    write_file(&prompt, "# System prompt\nReplace with your system prompt.\n")?;
    created.push(prompt);

    let config = pkg_dir.join("config/default-config.json");
    write_file(
        &config,
        "{\n  \"model\": \"gpt-4o\",\n  \"temperature\": 0.7,\n  \"max_tokens\": 4096\n}\n",
    )?;
    created.push(config);

    Ok(created)
}

fn scaffold_config_only(pkg_dir: &Path, _args: &ScaffoldArgs) -> Result<Vec<PathBuf>, String> {
    let mut created = Vec::new();

    let system_prompt = pkg_dir.join("assets/prompts/system.txt");
    write_file(
        &system_prompt,
        "# System prompt\nReplace with your agent's system prompt.\n",
    )?;
    created.push(system_prompt);

    let config = pkg_dir.join("config/default-config.json");
    write_file(
        &config,
        "{\n  \"model\": \"gpt-4o\",\n  \"temperature\": 0.7,\n  \"max_tokens\": 4096\n}\n",
    )?;
    created.push(config);

    Ok(created)
}

fn scaffold_workflow(pkg_dir: &Path, _args: &ScaffoldArgs) -> Result<Vec<PathBuf>, String> {
    let mut created = Vec::new();

    let flow_def = pkg_dir.join("flow/definition.json");
    write_file(
        &flow_def,
        "{\n  \"steps\": [],\n  \"timeout_secs\": 300,\n  \"on_timeout\": \"escalate\"\n}\n",
    )?;
    created.push(flow_def);

    let config = pkg_dir.join("config/default-config.json");
    write_file(
        &config,
        "{\n  \"triggers\": [],\n  \"escalation_target\": null\n}\n",
    )?;
    created.push(config);

    Ok(created)
}

fn build_package_json(args: &ScaffoldArgs) -> String {
    let runtime_base_field = match &args.runtime_base {
        Some(base) => format!("  \"runtime_base\": \"{base}\""),
        None => "  \"runtime_base\": null".to_string(),
    };

    let entry_point_field = match args.package_type {
        PackageType::FullRuntime => {
            "\n  \"entry_point\": \"bin/start.sh\","
        }
        _ => "",
    };

    let config_template_field = match args.package_type {
        PackageType::Workflow => "",
        _ => "\n  \"config_template\": \"config/default-config.json\",",
    };

    format!(
        "{{\n  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"type\": \"{pkg_type}\",\n  \"description\": \"\",{entry}{config}\n{base}\n}}\n",
        name = args.name,
        version = args.version,
        pkg_type = args.package_type.as_str(),
        entry = entry_point_field,
        config = config_template_field,
        base = runtime_base_field,
    )
}

fn run(args: ScaffoldArgs) -> Result<(), CliError> {
    let pkg_dir = args.output_dir.join(&args.name);

    if pkg_dir.exists() {
        return Err(format!(
            "directory already exists: '{}' — remove it or choose a different output dir",
            pkg_dir.display()
        )
        .into());
    }

    println!("fluxbee-scaffold v{}", env!("CARGO_PKG_VERSION"));
    println!("Scaffolding: {}", args.name);
    println!("  Type:    {}", args.package_type.label());
    println!("  Version: {}", args.version);
    if let Some(base) = &args.runtime_base {
        println!("  Base:    {base}");
    }
    println!("  Dir:     {}", pkg_dir.display());
    println!();

    fs::create_dir_all(&pkg_dir).map_err(|e| format!("create '{}': {e}", pkg_dir.display()))?;

    let package_json = build_package_json(&args);
    write_file(&pkg_dir.join("package.json"), &package_json)
        .map_err(|e| -> CliError { e.into() })?;

    let mut created = vec![pkg_dir.join("package.json")];

    let type_files = match args.package_type {
        PackageType::FullRuntime => scaffold_full_runtime(&pkg_dir, &args),
        PackageType::ConfigOnly => scaffold_config_only(&pkg_dir, &args),
        PackageType::Workflow => scaffold_workflow(&pkg_dir, &args),
    }
    .map_err(|e| -> CliError { e.into() })?;

    created.extend(type_files);

    for path in &created {
        let rel = path.strip_prefix(&args.output_dir).unwrap_or(path);
        println!("  created: {}", rel.display());
    }

    println!();
    println!("Done. Next steps:");
    match args.package_type {
        PackageType::FullRuntime => {
            println!("  1. Add your compiled binary as  {}/bin/start.sh", args.name);
            println!("  2. Edit assets and config as needed");
            println!(
                "  3. cd {} && zip -r ../{}-{}.zip .",
                args.name, args.name, args.version
            );
            println!(
                "  4. Upload the .zip via Archi Publish Package panel or fluxbee-publish ."
            );
        }
        PackageType::ConfigOnly | PackageType::Workflow => {
            println!("  1. Edit assets/config in {}/", args.name);
            println!(
                "  2. cd {} && zip -r ../{}-{}.zip .",
                args.name, args.name, args.version
            );
            println!("  3. Upload the .zip via Archi Publish Package panel");
        }
    }

    Ok(())
}

fn main() -> Result<(), CliError> {
    let args: Vec<String> = std::env::args().collect();
    let program = args
        .first()
        .map_or_else(|| "fluxbee-scaffold".to_string(), |v| v.clone());

    match parse_args(&args) {
        Ok(parsed) => run(parsed),
        Err(e) if e == "__help__" => {
            print!("{}", usage(&program));
            Ok(())
        }
        Err(e) => {
            eprintln!("Error: {e}");
            eprintln!();
            eprint!("{}", usage(&program));
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        std::iter::once("fluxbee-scaffold")
            .chain(v.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn parse_args_minimal_full_runtime() {
        let a = parse_args(&args(&["--name", "sy.node.test", "--type", "full_runtime"])).unwrap();
        assert_eq!(a.name, "sy.node.test");
        assert_eq!(a.version, "0.1.0");
        assert_eq!(a.package_type, PackageType::FullRuntime);
        assert!(a.runtime_base.is_none());
    }

    #[test]
    fn parse_args_config_only_requires_base() {
        let err = parse_args(&args(&["--name", "ai.billing", "--type", "config_only"]))
            .unwrap_err();
        assert!(err.contains("--base is required"), "err={err}");
    }

    #[test]
    fn parse_args_workflow_with_base_and_version() {
        let a = parse_args(&args(&[
            "--name", "wf.onboarding", "--type", "workflow",
            "--base", "wf.engine", "--version", "1.2.3",
        ]))
        .unwrap();
        assert_eq!(a.runtime_base.as_deref(), Some("wf.engine"));
        assert_eq!(a.version, "1.2.3");
    }

    #[test]
    fn parse_args_rejects_bad_name() {
        let err = parse_args(&args(&["--name", "AI.Bad", "--type", "full_runtime"])).unwrap_err();
        assert!(err.contains("naming policy"), "err={err}");
    }

    #[test]
    fn parse_args_rejects_bad_version() {
        let err = parse_args(&args(&["--name", "sy.ok", "--type", "full_runtime", "--version", "v1"]))
            .unwrap_err();
        assert!(err.contains("not valid semver"), "err={err}");
    }

    #[test]
    fn parse_args_custom_output_dir() {
        let a = parse_args(&args(&[
            "--name", "sy.ok", "--type", "full_runtime", "/tmp/out",
        ]))
        .unwrap();
        assert_eq!(a.output_dir, PathBuf::from("/tmp/out"));
    }

    #[test]
    fn run_scaffolds_full_runtime_structure() {
        let tmp = std::env::temp_dir()
            .join(format!("fluxbee-scaffold-test-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&tmp).unwrap();

        let a = ScaffoldArgs {
            name: "sy.scaffold.test".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::FullRuntime,
            runtime_base: None,
            output_dir: tmp.clone(),
        };
        run(a).expect("scaffold should succeed");

        let pkg = tmp.join("sy.scaffold.test");
        assert!(pkg.join("package.json").exists());
        assert!(pkg.join("bin/start.sh").exists());
        assert!(pkg.join("assets/prompts/system.txt").exists());
        assert!(pkg.join("config/default-config.json").exists());

        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(pkg.join("bin/start.sh")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);

        let pj: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(pkg.join("package.json")).unwrap()).unwrap();
        assert_eq!(pj["name"], "sy.scaffold.test");
        assert_eq!(pj["type"], "full_runtime");

        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn run_scaffolds_config_only_structure() {
        let tmp = std::env::temp_dir()
            .join(format!("fluxbee-scaffold-test-co-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&tmp).unwrap();

        let a = ScaffoldArgs {
            name: "ai.billing.test".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::ConfigOnly,
            runtime_base: Some("ai.generic".to_string()),
            output_dir: tmp.clone(),
        };
        run(a).expect("scaffold should succeed");

        let pkg = tmp.join("ai.billing.test");
        assert!(pkg.join("package.json").exists());
        assert!(pkg.join("assets/prompts/system.txt").exists());
        assert!(pkg.join("config/default-config.json").exists());
        assert!(!pkg.join("bin/start.sh").exists());

        let pj: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(pkg.join("package.json")).unwrap()).unwrap();
        assert_eq!(pj["runtime_base"], "ai.generic");

        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn run_scaffolds_workflow_structure() {
        let tmp = std::env::temp_dir()
            .join(format!("fluxbee-scaffold-test-wf-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&tmp).unwrap();

        let a = ScaffoldArgs {
            name: "wf.onboarding.test".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::Workflow,
            runtime_base: Some("wf.engine".to_string()),
            output_dir: tmp.clone(),
        };
        run(a).expect("scaffold should succeed");

        let pkg = tmp.join("wf.onboarding.test");
        assert!(pkg.join("package.json").exists());
        assert!(pkg.join("flow/definition.json").exists());
        assert!(pkg.join("config/default-config.json").exists());

        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn run_rejects_existing_directory() {
        let tmp = std::env::temp_dir()
            .join(format!("fluxbee-scaffold-test-exist-{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let pkg = tmp.join("sy.exists");
        fs::create_dir_all(&pkg).unwrap();

        let a = ScaffoldArgs {
            name: "sy.exists".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::FullRuntime,
            runtime_base: None,
            output_dir: tmp.clone(),
        };
        let err = run(a).unwrap_err();
        assert!(err.to_string().contains("already exists"), "err={err}");

        let _ = fs::remove_dir_all(tmp);
    }
}
