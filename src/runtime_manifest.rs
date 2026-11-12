use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    (RUNTIME_MANIFEST_SCHEMA_VERSION_MIN..=RUNTIME_MANIFEST_SCHEMA_VERSION_MAX)
        .contains(&schema_version)
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

/// How long a writer of a runtimes manifest waits for its lock (`lock_runtime_manifest`): above
/// the longest hold, SY.admin's `remove_runtime_version`, whose read of the version's use (30 s)
/// comes between its read of the manifest and its write.
pub const RUNTIME_MANIFEST_LOCK_WAIT: Duration = Duration::from_secs(60);

/// The name of the runtimes manifest's lock file, in the dist root (`runtime_manifest_lock_path`).
pub const RUNTIME_MANIFEST_LOCK_FILE: &str = "runtimes-manifest.lock";

/// The file every writer of a runtimes manifest locks for its read-modify-write (FINDINGS A-171):
/// `runtimes-manifest.lock` in the dist root, beside the `runtimes` folder that holds the manifest
/// (`<dist>/runtimes/manifest.json` → `<dist>/runtimes-manifest.lock`), never inside it: Syncthing
/// shares that folder with the workers, and the dist root itself is no folder of it. SY.admin
/// (`publish_runtime_package`, `remove_runtime_version`, `reset_base_runtime`), `fluxbee-publish`,
/// `scripts/publish-runtime.sh` and the .deb's `fluxbee-seed-runtimes` each hold one exclusive
/// flock on it from their read of the manifest to their write, so none loses another's change. The
/// file stays: removing it would let a writer lock a new one beside one still held.
pub fn runtime_manifest_lock_path(manifest_path: &Path) -> PathBuf {
    let runtimes_dir = manifest_path.parent().unwrap_or_else(|| Path::new(""));
    runtimes_dir
        .parent()
        .unwrap_or(runtimes_dir)
        .join(RUNTIME_MANIFEST_LOCK_FILE)
}

/// The runtimes manifest's lock, held until dropped: the kernel releases the flock with its
/// descriptor, however its holder ends.
#[derive(Debug)]
pub struct RuntimeManifestLock {
    _file: fs::File,
}

/// How `lock_runtime_manifest`'s error begins when another writer held the lock past the wait.
const RUNTIME_MANIFEST_LOCK_HELD: &str = "runtime manifest lock: another writer held";

/// Whether `err`, an error of `lock_runtime_manifest`, says another writer held the lock past the
/// wait (FINDINGS A-305): nothing was read or written, and asking again once that writer is done
/// may pass. Any other error of the lock (its file cannot be opened or locked) is the dist's to fix.
pub fn runtime_manifest_lock_held_too_long(err: &str) -> bool {
    err.contains(RUNTIME_MANIFEST_LOCK_HELD)
}

/// Takes the lock of the runtimes manifest at `manifest_path` (`runtime_manifest_lock_path`),
/// waiting up to `wait` while another writer holds it; past that, an error that says so.
pub fn lock_runtime_manifest(
    manifest_path: &Path,
    wait: Duration,
) -> Result<RuntimeManifestLock, String> {
    let path = runtime_manifest_lock_path(manifest_path);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(&path)
        .map_err(|err| {
            format!(
                "runtime manifest lock: open '{}' failed: {}",
                path.display(),
                err
            )
        })?;
    let deadline = Instant::now() + wait;
    loop {
        // SAFETY: the descriptor is open and owned by `file` for the whole call.
        if unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) }
            == 0
        {
            return Ok(RuntimeManifestLock { _file: file });
        }
        let err = std::io::Error::last_os_error();
        if !matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) {
            return Err(format!(
                "runtime manifest lock: flock '{}' failed: {}",
                path.display(),
                err
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{RUNTIME_MANIFEST_LOCK_HELD} '{}' for {}s",
                path.display(),
                wait.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
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

    /// FINDINGS A-171: one writer of the manifest at a time. While one holds its lock, in the dist
    /// root beside the synced runtimes folder, another waits, and past its wait gives up and says
    /// so; once the first is done, the one waiting takes it. The lock file stays, and nothing is
    /// written in the runtimes folder.
    #[test]
    fn a_second_writer_waits_for_the_manifests_lock() {
        let dir = std::env::temp_dir().join(format!("runtime-manifest-lock-{}", Uuid::new_v4()));
        let runtimes = dir.join("runtimes");
        fs::create_dir_all(&runtimes).expect("create dir");
        let manifest = runtimes.join("manifest.json");
        assert_eq!(
            runtime_manifest_lock_path(&manifest),
            dir.join("runtimes-manifest.lock")
        );
        let first = lock_runtime_manifest(&manifest, Duration::ZERO).expect("the first");
        let refused = lock_runtime_manifest(&manifest, Duration::from_millis(200))
            .expect_err("held by the first");
        assert!(refused.contains("another writer held"), "{refused}");

        let waiting = std::thread::spawn({
            let manifest = manifest.clone();
            move || {
                let started = Instant::now();
                lock_runtime_manifest(&manifest, Duration::from_secs(10))
                    .map(|lock| (lock, started.elapsed()))
            }
        });
        std::thread::sleep(Duration::from_millis(300));
        drop(first);
        let (second, waited) = waiting.join().expect("joined").expect("the second");
        assert!(waited >= Duration::from_millis(250), "{waited:?}");
        drop(second);
        assert!(runtime_manifest_lock_path(&manifest).is_file());
        assert_eq!(fs::read_dir(&runtimes).expect("runtimes").count(), 0);
        let _ = fs::remove_dir_all(dir);
    }
}
