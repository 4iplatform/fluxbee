use std::fs;
use std::io::Write as IoWrite;
use std::os::unix::fs::PermissionsExt;
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

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "full_runtime" => Some(PackageType::FullRuntime),
            "config_only" => Some(PackageType::ConfigOnly),
            "workflow" => Some(PackageType::Workflow),
            _ => None,
        }
    }
}

// ── Subcommand dispatch ──────────────────────────────────────────────────────

#[derive(Debug)]
enum Subcommand {
    New(ScaffoldArgs),
    Pack(PackArgs),
}

// ── new ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ScaffoldArgs {
    name: String,
    version: String,
    package_type: PackageType,
    runtime_base: Option<String>,
    output_dir: PathBuf,
}

// ── pack ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct PackArgs {
    /// Directory that contains package.json (already has correct layout).
    package_dir: PathBuf,
    /// For full_runtime: path to the compiled binary to copy as bin/start.sh.
    binary: Option<PathBuf>,
    /// Where to write the zip (defaults to ../<name>-<version>.zip).
    output: Option<PathBuf>,
}

// ── helpers ──────────────────────────────────────────────────────────────────

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
    fs::write(path, content).map_err(|e| format!("failed to write '{}': {e}", path.display()))?;
    Ok(())
}

fn read_package_json(dir: &Path) -> Result<serde_json::Value, String> {
    let path = dir.join("package.json");
    let raw =
        fs::read_to_string(&path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("invalid JSON in '{}': {e}", path.display()))
}

// ── usage ────────────────────────────────────────────────────────────────────

fn usage(program: &str) -> String {
    format!(
        "\
Usage:
  {program} new  --name <runtime_name> --type <package_type> [options] [output_dir]
  {program} pack <package_dir> [--binary <path>] [--output <zip_path>]
  {program} --help

Subcommands:
  new   Scaffold a new package directory with placeholder files
  pack  Zip an existing package directory ready for upload

new options:
  --name <runtime_name>   Runtime name (lowercase, dots, e.g. ai.soporte.billing)
  --type <package_type>   One of: full_runtime, config_only, workflow
  --version <semver>      Initial version (default: 0.1.0)
  --base <runtime_name>   Base runtime (required for config_only and workflow)
  output_dir              Where to create the package dir (default: .)

pack options:
  --binary <path>         Compiled binary to copy into bin/start.sh before zipping
                          (required for full_runtime if bin/start.sh is a placeholder)
  --output <zip_path>     Output zip path (default: ./<name>-<version>.zip next to package_dir)

Examples:
  # Create a new config-only agent
  {program} new --name ai.soporte.billing --type config_only --base ai.generic

  # Create a new full runtime
  {program} new --name sy.frontdesk.gov --type full_runtime --version 1.0.0

  # Re-zip an existing package (config_only / workflow — no binary needed)
  {program} pack ./ai.soporte.billing

  # Re-zip a full_runtime after recompiling
  {program} pack ./sy.frontdesk.gov --binary ./target/release/sy-frontdesk-gov

  # Zip with explicit output path
  {program} pack ./ai.soporte.billing --output ./dist/ai.soporte.billing-1.0.0.zip
"
    )
}

// ── arg parsing ──────────────────────────────────────────────────────────────

fn parse_new_args(args: &[String]) -> Result<ScaffoldArgs, String> {
    let mut name: Option<String> = None;
    let mut version = "0.1.0".to_string();
    let mut package_type: Option<PackageType> = None;
    let mut runtime_base: Option<String> = None;
    let mut output_dir: Option<PathBuf> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
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
                package_type = Some(PackageType::from_str(v).ok_or_else(|| {
                    format!(
                        "unknown package type: '{v}' (valid: full_runtime, config_only, workflow)"
                    )
                })?);
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

    if matches!(
        package_type,
        PackageType::ConfigOnly | PackageType::Workflow
    ) && runtime_base.is_none()
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

fn parse_pack_args(args: &[String]) -> Result<PackArgs, String> {
    let mut package_dir: Option<PathBuf> = None;
    let mut binary: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--binary" => {
                let v = args.get(i + 1).ok_or("missing value for --binary")?;
                binary = Some(PathBuf::from(v));
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("missing value for --output")?;
                output = Some(PathBuf::from(v));
                i += 2;
            }
            t if t.starts_with('-') => return Err(format!("unknown option: {t}")),
            _ => {
                if package_dir.is_some() {
                    return Err(format!("unexpected extra argument: {}", args[i]));
                }
                package_dir = Some(PathBuf::from(&args[i]));
                i += 1;
            }
        }
    }

    let package_dir = package_dir.ok_or("missing required <package_dir>")?;
    Ok(PackArgs {
        package_dir,
        binary,
        output,
    })
}

fn parse_args(args: &[String]) -> Result<Subcommand, String> {
    if args.is_empty() {
        return Err("missing subcommand".to_string());
    }

    let i = 1usize;
    // skip the program name, find the subcommand
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Err("__help__".to_string()),
            "new" => return parse_new_args(&args[i + 1..]).map(Subcommand::New),
            "pack" => return parse_pack_args(&args[i + 1..]).map(Subcommand::Pack),
            t if t.starts_with('-') => {
                // top-level flags that look like new's flags — treat as implicit "new"
                return parse_new_args(&args[i..]).map(Subcommand::New);
            }
            _ => {
                return Err(format!(
                    "unknown subcommand: '{}' (use 'new' or 'pack')",
                    args[i]
                ))
            }
        }
    }

    Err("missing subcommand".to_string())
}

// ── new ──────────────────────────────────────────────────────────────────────

fn scaffold_full_runtime(pkg_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut created = Vec::new();

    let start_sh = pkg_dir.join("bin/start.sh");
    write_file(
        &start_sh,
        "#!/usr/bin/env bash\n# Entry point — replace with your compiled binary\nexec \"$(dirname \"$0\")/node\" \"$@\"\n",
    )?;
    let mut perms = fs::metadata(&start_sh)
        .map_err(|e| format!("stat bin/start.sh: {e}"))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&start_sh, perms).map_err(|e| format!("chmod bin/start.sh: {e}"))?;
    created.push(start_sh);

    let prompt = pkg_dir.join("assets/prompts/system.txt");
    write_file(
        &prompt,
        "# System prompt\nReplace with your system prompt.\n",
    )?;
    created.push(prompt);

    let config = pkg_dir.join("config/default-config.json");
    write_file(
        &config,
        "{\n  \"model\": \"gpt-4o\",\n  \"temperature\": 0.7,\n  \"max_tokens\": 4096\n}\n",
    )?;
    created.push(config);

    Ok(created)
}

fn scaffold_config_only(pkg_dir: &Path) -> Result<Vec<PathBuf>, String> {
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

fn scaffold_workflow(pkg_dir: &Path) -> Result<Vec<PathBuf>, String> {
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
        PackageType::FullRuntime => "\n  \"entry_point\": \"bin/start.sh\",",
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

fn run_new(args: ScaffoldArgs) -> Result<(), CliError> {
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
        PackageType::FullRuntime => scaffold_full_runtime(&pkg_dir),
        PackageType::ConfigOnly => scaffold_config_only(&pkg_dir),
        PackageType::Workflow => scaffold_workflow(&pkg_dir),
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
            println!(
                "  1. Add your compiled binary as  {}/bin/start.sh",
                args.name
            );
            println!("  2. Edit assets and config as needed");
            println!(
                "  3. fluxbee-scaffold pack ./{} --binary ./target/release/<binary>",
                args.name
            );
        }
        PackageType::ConfigOnly | PackageType::Workflow => {
            println!("  1. Edit assets/config in {}/", args.name);
            println!("  2. fluxbee-scaffold pack ./{}", args.name);
        }
    }

    Ok(())
}

// ── pack ─────────────────────────────────────────────────────────────────────

fn zip_dir_into(src: &Path, zip_writer: &mut zip::ZipWriter<fs::File>) -> Result<u64, String> {
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let exe_options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    let mut file_count = 0u64;

    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("read_dir '{}': {e}", dir.display()))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
            let path = entry.path();
            let meta =
                fs::metadata(&path).map_err(|e| format!("metadata '{}': {e}", path.display()))?;

            // relative path inside the zip (no leading ./)
            let rel = path
                .strip_prefix(src)
                .map_err(|e| format!("strip_prefix: {e}"))?;
            let zip_path = rel.to_string_lossy().into_owned();

            if meta.is_dir() {
                stack.push(path);
            } else {
                let is_exe = meta.permissions().mode() & 0o111 != 0;
                let opts = if is_exe { exe_options } else { options };
                zip_writer
                    .start_file(&zip_path, opts)
                    .map_err(|e| format!("zip start_file '{}': {e}", zip_path))?;
                let bytes =
                    fs::read(&path).map_err(|e| format!("read '{}': {e}", path.display()))?;
                zip_writer
                    .write_all(&bytes)
                    .map_err(|e| format!("zip write '{}': {e}", zip_path))?;
                file_count += 1;
            }
        }
    }
    Ok(file_count)
}

fn run_pack(args: PackArgs) -> Result<(), CliError> {
    let pkg_dir = args.package_dir.canonicalize().map_err(|e| {
        format!(
            "package dir '{}' not found: {e}",
            args.package_dir.display()
        )
    })?;

    // read package.json to extract name + version
    let pj = read_package_json(&pkg_dir).map_err(|e| -> CliError { e.into() })?;
    let name = pj
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("package.json missing 'name'")?
        .to_string();
    let version = pj
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or("package.json missing 'version'")?
        .to_string();
    let pkg_type_str = pj
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("full_runtime");
    let pkg_type = PackageType::from_str(pkg_type_str)
        .ok_or_else(|| format!("unknown package type in package.json: '{pkg_type_str}'"))?;

    println!("fluxbee-scaffold v{}", env!("CARGO_PKG_VERSION"));
    println!("Packing: {name}  v{version}  [{pkg_type_str}]");

    // for full_runtime: optionally update bin/start.sh from --binary
    if let Some(binary_src) = &args.binary {
        if !binary_src.exists() {
            return Err(format!("binary not found: '{}'", binary_src.display()).into());
        }
        let bin_dir = pkg_dir.join("bin");
        fs::create_dir_all(&bin_dir).map_err(|e| format!("create bin/: {e}"))?;
        let start_sh = bin_dir.join("start.sh");
        fs::copy(binary_src, &start_sh)
            .map_err(|e| format!("copy '{}' → 'bin/start.sh': {e}", binary_src.display()))?;
        let mut perms = fs::metadata(&start_sh)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&start_sh, perms)?;
        println!("  binary:  {} → bin/start.sh", binary_src.display());
    }

    // validate minimum structure
    match pkg_type {
        PackageType::FullRuntime => {
            let start_sh = pkg_dir.join("bin/start.sh");
            if !start_sh.exists() {
                return Err(
                    "bin/start.sh not found — pass --binary <path> to copy your compiled binary"
                        .into(),
                );
            }
            let mode = fs::metadata(&start_sh)?.permissions().mode();
            if mode & 0o111 == 0 {
                return Err("bin/start.sh is not executable — run: chmod +x bin/start.sh".into());
            }
        }
        PackageType::Workflow => {
            if !pkg_dir.join("flow").exists() {
                return Err("flow/ directory not found (required for workflow packages)".into());
            }
        }
        PackageType::ConfigOnly => {}
    }

    // determine output zip path
    let zip_path = match &args.output {
        Some(p) => p.clone(),
        None => {
            let parent = pkg_dir.parent().unwrap_or_else(|| Path::new("."));
            parent.join(format!("{name}-{version}.zip"))
        }
    };

    let zip_file =
        fs::File::create(&zip_path).map_err(|e| format!("create '{}': {e}", zip_path.display()))?;
    let mut zip_writer = zip::ZipWriter::new(zip_file);
    let file_count =
        zip_dir_into(&pkg_dir, &mut zip_writer).map_err(|e| -> CliError { e.into() })?;
    zip_writer
        .finish()
        .map_err(|e| format!("zip finish: {e}"))?;

    let zip_bytes = fs::metadata(&zip_path)?.len();
    println!(
        "  zip:     {} ({} files, {} KB)",
        zip_path.display(),
        file_count,
        zip_bytes / 1024
    );
    println!();
    println!(
        "Done. Upload {} via Archi Publish Package panel.",
        zip_path.display()
    );

    Ok(())
}

// ── main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<(), CliError> {
    let args: Vec<String> = std::env::args().collect();
    let program = args
        .first()
        .map_or_else(|| "fluxbee-scaffold".to_string(), |v| v.clone());

    match parse_args(&args) {
        Ok(Subcommand::New(a)) => run_new(a),
        Ok(Subcommand::Pack(a)) => run_pack(a),
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

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn prog_args(v: &[&str]) -> Vec<String> {
        std::iter::once("fluxbee-scaffold")
            .chain(v.iter().copied())
            .map(str::to_string)
            .collect()
    }

    fn nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn tmp(prefix: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("{prefix}-{}", nanos()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ── new parsing ──────────────────────────────────────────────────────────

    #[test]
    fn parse_new_minimal_full_runtime() {
        let a = parse_args(&prog_args(&[
            "new",
            "--name",
            "sy.node.test",
            "--type",
            "full_runtime",
        ]))
        .unwrap();
        let Subcommand::New(a) = a else {
            panic!("expected New")
        };
        assert_eq!(a.name, "sy.node.test");
        assert_eq!(a.version, "0.1.0");
        assert_eq!(a.package_type, PackageType::FullRuntime);
    }

    #[test]
    fn parse_new_implicit_subcommand_compatibility() {
        // old-style: no "new" keyword, flags directly
        let a = parse_args(&prog_args(&[
            "--name",
            "sy.node.test",
            "--type",
            "full_runtime",
        ]))
        .unwrap();
        let Subcommand::New(a) = a else {
            panic!("expected New")
        };
        assert_eq!(a.name, "sy.node.test");
    }

    #[test]
    fn parse_new_config_only_requires_base() {
        let err = parse_args(&prog_args(&[
            "new",
            "--name",
            "ai.billing",
            "--type",
            "config_only",
        ]))
        .unwrap_err();
        assert!(err.contains("--base is required"), "err={err}");
    }

    #[test]
    fn parse_new_rejects_bad_name() {
        let err = parse_args(&prog_args(&[
            "new",
            "--name",
            "AI.Bad",
            "--type",
            "full_runtime",
        ]))
        .unwrap_err();
        assert!(err.contains("naming policy"), "err={err}");
    }

    #[test]
    fn parse_new_rejects_bad_version() {
        let err = parse_args(&prog_args(&[
            "new",
            "--name",
            "sy.ok",
            "--type",
            "full_runtime",
            "--version",
            "v1",
        ]))
        .unwrap_err();
        assert!(err.contains("not valid semver"), "err={err}");
    }

    // ── pack parsing ─────────────────────────────────────────────────────────

    #[test]
    fn parse_pack_minimal() {
        let a = parse_args(&prog_args(&["pack", "./some/dir"])).unwrap();
        let Subcommand::Pack(a) = a else {
            panic!("expected Pack")
        };
        assert_eq!(a.package_dir, PathBuf::from("./some/dir"));
        assert!(a.binary.is_none());
        assert!(a.output.is_none());
    }

    #[test]
    fn parse_pack_with_binary_and_output() {
        let a = parse_args(&prog_args(&[
            "pack",
            "./pkg",
            "--binary",
            "./target/release/my-bin",
            "--output",
            "/tmp/my-pkg.zip",
        ]))
        .unwrap();
        let Subcommand::Pack(a) = a else {
            panic!("expected Pack")
        };
        assert_eq!(a.binary.unwrap(), PathBuf::from("./target/release/my-bin"));
        assert_eq!(a.output.unwrap(), PathBuf::from("/tmp/my-pkg.zip"));
    }

    #[test]
    fn parse_pack_missing_dir() {
        let err = parse_args(&prog_args(&["pack"])).unwrap_err();
        assert!(err.contains("missing required <package_dir>"), "err={err}");
    }

    // ── new E2E ──────────────────────────────────────────────────────────────

    #[test]
    fn run_new_scaffolds_full_runtime() {
        let out = tmp("scaffold-new-full");
        run_new(ScaffoldArgs {
            name: "sy.scaffold.test".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::FullRuntime,
            runtime_base: None,
            output_dir: out.clone(),
        })
        .unwrap();

        let pkg = out.join("sy.scaffold.test");
        assert!(pkg.join("package.json").exists());
        assert!(pkg.join("bin/start.sh").exists());
        assert!(pkg.join("assets/prompts/system.txt").exists());
        assert!(pkg.join("config/default-config.json").exists());

        let mode = fs::metadata(pkg.join("bin/start.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);

        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn run_new_scaffolds_config_only() {
        let out = tmp("scaffold-new-co");
        run_new(ScaffoldArgs {
            name: "ai.billing.test".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::ConfigOnly,
            runtime_base: Some("ai.generic".to_string()),
            output_dir: out.clone(),
        })
        .unwrap();

        let pkg = out.join("ai.billing.test");
        assert!(pkg.join("package.json").exists());
        assert!(!pkg.join("bin/start.sh").exists());
        let pj: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(pkg.join("package.json")).unwrap()).unwrap();
        assert_eq!(pj["runtime_base"], "ai.generic");

        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn run_new_rejects_existing_directory() {
        let out = tmp("scaffold-exist");
        fs::create_dir_all(out.join("sy.exists")).unwrap();
        let err = run_new(ScaffoldArgs {
            name: "sy.exists".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::FullRuntime,
            runtime_base: None,
            output_dir: out.clone(),
        })
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "err={err}");
        let _ = fs::remove_dir_all(out);
    }

    // ── pack E2E ─────────────────────────────────────────────────────────────

    #[test]
    fn run_pack_config_only_produces_zip() {
        let out = tmp("scaffold-pack-co");

        // scaffold first
        run_new(ScaffoldArgs {
            name: "ai.pack.test".to_string(),
            version: "1.2.3".to_string(),
            package_type: PackageType::ConfigOnly,
            runtime_base: Some("ai.generic".to_string()),
            output_dir: out.clone(),
        })
        .unwrap();

        let pkg_dir = out.join("ai.pack.test");
        let zip_path = out.join("ai.pack.test-1.2.3.zip");

        run_pack(PackArgs {
            package_dir: pkg_dir.clone(),
            binary: None,
            output: Some(zip_path.clone()),
        })
        .unwrap();

        assert!(zip_path.exists());
        assert!(zip_path.metadata().unwrap().len() > 0);

        // verify zip contains package.json
        let mut archive = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "package.json"), "names={names:?}");
        assert!(
            names.iter().any(|n| n.contains("system.txt")),
            "names={names:?}"
        );

        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn run_pack_full_runtime_with_binary() {
        let out = tmp("scaffold-pack-full");

        run_new(ScaffoldArgs {
            name: "sy.pack.full".to_string(),
            version: "0.1.0".to_string(),
            package_type: PackageType::FullRuntime,
            runtime_base: None,
            output_dir: out.clone(),
        })
        .unwrap();

        // create a fake compiled binary
        let fake_bin = out.join("fake-binary");
        fs::write(&fake_bin, "#!/usr/bin/env bash\necho ok\n").unwrap();
        let mut perms = fs::metadata(&fake_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_bin, perms).unwrap();

        let zip_path = out.join("sy.pack.full-0.1.0.zip");
        run_pack(PackArgs {
            package_dir: out.join("sy.pack.full"),
            binary: Some(fake_bin),
            output: Some(zip_path.clone()),
        })
        .unwrap();

        assert!(zip_path.exists());
        let mut archive = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "bin/start.sh"), "names={names:?}");

        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn run_pack_full_runtime_missing_start_sh_without_binary() {
        let out = tmp("scaffold-pack-missing");

        // manually create a full_runtime dir without bin/start.sh
        let pkg = out.join("sy.no.start");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(
            pkg.join("package.json"),
            r#"{"name":"sy.no.start","version":"0.1.0","type":"full_runtime"}"#,
        )
        .unwrap();

        let err = run_pack(PackArgs {
            package_dir: pkg,
            binary: None,
            output: None,
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("bin/start.sh not found"),
            "err={err}"
        );

        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn run_pack_default_zip_name_adjacent_to_package_dir() {
        let out = tmp("scaffold-pack-default-name");

        run_new(ScaffoldArgs {
            name: "ai.default.name".to_string(),
            version: "2.0.0".to_string(),
            package_type: PackageType::ConfigOnly,
            runtime_base: Some("ai.generic".to_string()),
            output_dir: out.clone(),
        })
        .unwrap();

        run_pack(PackArgs {
            package_dir: out.join("ai.default.name"),
            binary: None,
            output: None,
        })
        .unwrap();

        // default zip should land next to the package dir
        assert!(out.join("ai.default.name-2.0.0.zip").exists());

        let _ = fs::remove_dir_all(out);
    }
}
