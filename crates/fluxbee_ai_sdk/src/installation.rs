//! The installation file (docs/hive-config-update-v1.md §4): the operator's settings for the whole
//! installation, today `ai`. The motherbee's orchestrator writes it, the only writer, and every hive
//! receives it through dist, so every hive reads the same version. A hive whose dist does not hold
//! it yet reads `ai` from its own hive.yaml, as before.
//!
//! The format is read by every hive's readers, ai.generic runtimes on an older SDK among them: a
//! new `schema_version` waits until all of them read it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::{AiSdkError, Result};
use crate::llm::{AiProviderModelConfig, HiveAiConfig};

/// Where the installation file sits inside a hive's dist (`dist.path` in its hive.yaml).
pub const INSTALLATION_FILE_IN_DIST: &str = "installation/installation.json";

/// The installation file under the default dist, `/var/lib/fluxbee/dist`.
pub const INSTALLATION_FILE_PATH: &str = "/var/lib/fluxbee/dist/installation/installation.json";

/// The file's format.
pub const INSTALLATION_SCHEMA_VERSION: u32 = 1;

/// The installation file under a hive's dist root.
pub fn installation_file_path(dist_root: &Path) -> PathBuf {
    dist_root.join(INSTALLATION_FILE_IN_DIST)
}

/// The operator's settings, one version for the whole installation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallationFile {
    pub schema_version: u32,
    /// Raised by one at each operator change; 1 when the motherbee first writes it.
    pub version: u64,
    /// The default provider and each provider's model. None: the fallback engine
    /// (`HiveAiConfig::fallback`), as a hive.yaml without `ai`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<HiveAiConfig>,
}

impl InstallationFile {
    /// What every reader needs of it: this release's schema, a version of 1 or more, and an `ai`
    /// whose default provider has its model (`HiveAiConfig::effective`) and whose every provider
    /// named has a model of its own, since an ai.generic node may pick any of them.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != INSTALLATION_SCHEMA_VERSION {
            return Err(invalid(format!(
                "schema_version {} (this release reads {INSTALLATION_SCHEMA_VERSION})",
                self.schema_version
            )));
        }
        if self.version == 0 {
            return Err(invalid("version 0 (the first is 1)".to_string()));
        }
        if let Some(ai) = &self.ai {
            ai.effective()
                .map_err(|err| invalid(format!("its ai: {}", plain(err))))?;
            for (name, provider) in [
                ("openai", &ai.providers.openai),
                ("anthropic", &ai.providers.anthropic),
            ] {
                if provider
                    .as_ref()
                    .is_some_and(|AiProviderModelConfig { model }| model.trim().is_empty())
                {
                    return Err(invalid(format!("its ai: providers.{name}.model is empty")));
                }
            }
        }
        Ok(())
    }

    /// A file every reader can use (`validate`).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let file: InstallationFile = serde_json::from_slice(bytes)
            .map_err(|err| invalid(format!("it does not parse: {err}")))?;
        file.validate()?;
        Ok(file)
    }

    /// The bytes the motherbee writes for it, once `validate` passes: pretty JSON and one newline.
    /// Its hash is the hash of these bytes as written; nobody re-renders a file to compare it.
    pub fn render(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// The installation file a hive holds, and the sha256 of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledSettings {
    pub file: InstallationFile,
    pub sha256: String,
}

/// What a hive holds at the installation file's place, for `/versions` and its readers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallationFileState {
    /// Its dist does not hold one yet.
    Absent,
    /// A file every reader can use.
    Valid(InstalledSettings),
    /// A file that is there and that no reader can use (a hand edit, say): the hash of its bytes,
    /// and why.
    Invalid { sha256: String, error: String },
}

/// What the hive holds at `path`. An error only when the path cannot be read at all (not found is
/// `Absent`).
pub fn installation_file_state(path: &Path) -> Result<InstallationFileState> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstallationFileState::Absent)
        }
        Err(err) => return Err(invalid(format!("'{}' unreadable: {err}", path.display()))),
    };
    let sha256 = sha256_hex(&bytes);
    Ok(match InstallationFile::parse(&bytes) {
        Ok(file) => InstallationFileState::Valid(InstalledSettings { file, sha256 }),
        Err(err) => InstallationFileState::Invalid {
            sha256,
            error: plain(err),
        },
    })
}

/// The installation file at `path`: None while the hive's dist does not hold one. A file that is
/// there and is not a valid one is an error.
pub fn read_installation_file(path: &Path) -> Result<Option<InstalledSettings>> {
    match installation_file_state(path)? {
        InstallationFileState::Absent => Ok(None),
        InstallationFileState::Valid(installed) => Ok(Some(installed)),
        InstallationFileState::Invalid { error, .. } => Err(AiSdkError::Protocol(error)),
    }
}

/// Where a reader's `ai` came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiSettingsSource {
    /// The installation file, at this version and hash.
    Installation { version: u64, sha256: String },
    /// The hive's own hive.yaml: its dist holds no installation file yet. A reader says so.
    HiveYaml,
}

/// A reader's `ai`, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSettings {
    pub ai: Option<HiveAiConfig>,
    pub source: AiSettingsSource,
}

/// A reader's `ai`, from the installation file under the default dist (`ai_settings_at`).
pub fn ai_settings<F>(hive_yaml_ai: F) -> Result<AiSettings>
where
    F: FnOnce() -> Result<Option<HiveAiConfig>>,
{
    ai_settings_at(Path::new(INSTALLATION_FILE_PATH), hive_yaml_ai)
}

/// A reader's `ai`: the installation file's at `path` once the hive holds one, and the hive's own
/// hive.yaml's until then, which `hive_yaml_ai` reads only in that case. A file that is there and
/// is not a valid one is an error, never a fallback to a hive.yaml that may be older: whether a
/// reader stops on it or reports it and goes on is the reader's call.
pub fn ai_settings_at<F>(path: &Path, hive_yaml_ai: F) -> Result<AiSettings>
where
    F: FnOnce() -> Result<Option<HiveAiConfig>>,
{
    Ok(match read_installation_file(path)? {
        Some(installed) => AiSettings {
            ai: installed.file.ai,
            source: AiSettingsSource::Installation {
                version: installed.file.version,
                sha256: installed.sha256,
            },
        },
        None => AiSettings {
            ai: hive_yaml_ai()?,
            source: AiSettingsSource::HiveYaml,
        },
    })
}

/// The sha256 of `bytes`, lowercase hex: the hash `/versions` and the update compare.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid(detail: String) -> AiSdkError {
    AiSdkError::Protocol(format!("installation file: {detail}"))
}

/// An error's own words, without the "protocol error:" its Display adds.
fn plain(err: AiSdkError) -> String {
    match err {
        AiSdkError::Protocol(detail) => detail,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{AiProvider, AiProviderConfigs};

    fn both_providers() -> HiveAiConfig {
        HiveAiConfig {
            default_provider: AiProvider::Anthropic,
            providers: AiProviderConfigs {
                openai: Some(AiProviderModelConfig {
                    model: "gpt-5.5".to_string(),
                }),
                anthropic: Some(AiProviderModelConfig {
                    model: "claude-sonnet-5-5".to_string(),
                }),
            },
        }
    }

    fn temp_dist(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fbinst-{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("installation")).expect("temp dist");
        dir
    }

    #[test]
    fn a_file_the_motherbee_renders_reads_back_the_same() {
        let file = InstallationFile {
            schema_version: INSTALLATION_SCHEMA_VERSION,
            version: 1,
            ai: Some(both_providers()),
        };
        let bytes = file.render().expect("renders");
        assert!(bytes.ends_with(b"}\n"));
        assert_eq!(InstallationFile::parse(&bytes).expect("parses"), file);
        let no_ai = InstallationFile {
            schema_version: INSTALLATION_SCHEMA_VERSION,
            version: 3,
            ai: None,
        };
        let bytes = no_ai.render().expect("renders");
        assert!(!String::from_utf8_lossy(&bytes).contains("\"ai\""));
        assert_eq!(InstallationFile::parse(&bytes).expect("parses"), no_ai);
    }

    #[test]
    fn a_file_no_reader_can_use_is_refused_read_and_written() {
        let refused = [
            r#"{"schema_version":2,"version":1}"#,
            r#"{"schema_version":1,"version":0}"#,
            r#"{"schema_version":1}"#,
            r#"{"schema_version":1,"version":1,"ai":{"default_provider":"anthropic","providers":{"openai":{"model":"gpt-5.5"}}}}"#,
            r#"{"schema_version":1,"version":1,"ai":{"default_provider":"openai","providers":{"openai":{"model":"  "}}}}"#,
            r#"{"schema_version":1,"version":1,"ai":{"default_provider":"anthropic","providers":{"openai":{"model":" "},"anthropic":{"model":"claude-sonnet-5-5"}}}}"#,
            "not json",
        ];
        for raw in refused {
            let err = InstallationFile::parse(raw.as_bytes()).expect_err(raw);
            let text = err.to_string();
            assert!(text.contains("installation file"), "{text}");
            assert_eq!(text.matches("protocol error").count(), 1, "{text}");
        }
        let zero = InstallationFile {
            schema_version: INSTALLATION_SCHEMA_VERSION,
            version: 0,
            ai: None,
        };
        assert!(zero.render().is_err(), "the writer cannot write one either");
    }

    #[test]
    fn a_reader_takes_the_installation_file_once_its_hive_holds_one() {
        let dist = temp_dist("settings");
        let path = installation_file_path(&dist);
        let hive_yaml = HiveAiConfig {
            default_provider: AiProvider::OpenAi,
            providers: AiProviderConfigs {
                openai: Some(AiProviderModelConfig {
                    model: "gpt-5.5".to_string(),
                }),
                anthropic: None,
            },
        };
        // No file yet: the hive's own hive.yaml, as before.
        let before = ai_settings_at(&path, || Ok(Some(hive_yaml.clone()))).expect("no file");
        assert_eq!(before.source, AiSettingsSource::HiveYaml);
        assert_eq!(before.ai.as_ref(), Some(&hive_yaml));
        assert_eq!(
            installation_file_state(&path).expect("read"),
            InstallationFileState::Absent
        );
        // The file: its ai, its version and the hash of its bytes; hive.yaml is not even read.
        let file = InstallationFile {
            schema_version: INSTALLATION_SCHEMA_VERSION,
            version: 4,
            ai: Some(both_providers()),
        };
        let bytes = file.render().expect("renders");
        std::fs::write(&path, &bytes).expect("written");
        let after = ai_settings_at(&path, || {
            Err(AiSdkError::Protocol(
                "hive.yaml's ai is not read".to_string(),
            ))
        })
        .expect("the file");
        assert_eq!(after.ai, Some(both_providers()));
        assert_eq!(
            after.source,
            AiSettingsSource::Installation {
                version: 4,
                sha256: sha256_hex(&bytes),
            }
        );
        // A file that is there and is not a valid one is an error, not hive.yaml's ai, and its
        // state names its hash for /versions.
        let edited = b"{\"schema_version\":1,\"version\":0}";
        std::fs::write(&path, edited).expect("edited");
        assert!(ai_settings_at(&path, || Ok(Some(hive_yaml.clone()))).is_err());
        match installation_file_state(&path).expect("read") {
            InstallationFileState::Invalid { sha256, error } => {
                assert_eq!(sha256, sha256_hex(edited));
                assert!(error.contains("version 0"), "{error}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dist);
    }

    #[test]
    fn the_file_sits_in_the_hives_dist_and_its_hash_is_the_sha256_of_its_bytes() {
        assert_eq!(
            installation_file_path(Path::new("/var/lib/fluxbee/dist")),
            PathBuf::from(INSTALLATION_FILE_PATH)
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
