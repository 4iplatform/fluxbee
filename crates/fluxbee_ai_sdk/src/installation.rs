//! The installation file (docs/hive-config-update-v1.md §4): the operator's settings for the whole
//! installation, today `ai`. The motherbee's orchestrator writes it, the only writer, and every hive
//! receives it through dist, so every hive reads the same version. A hive whose dist does not hold
//! it yet reads `ai` from its own hive.yaml, as before.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::{AiSdkError, Result};
use crate::llm::HiveAiConfig;

/// Where every hive finds the installation file: the dist folder the motherbee sends.
pub const INSTALLATION_FILE_PATH: &str = "/var/lib/fluxbee/dist/installation/installation.json";

/// The file's format.
pub const INSTALLATION_SCHEMA_VERSION: u32 = 1;

/// The operator's settings, one version for the whole installation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallationFile {
    pub schema_version: u32,
    /// Raised by one at each operator change; 1 when the motherbee first writes it.
    pub version: u64,
    /// The default provider and each provider's model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<HiveAiConfig>,
}

impl InstallationFile {
    /// A file every reader can use: its schema, a version of 1 or more, and an `ai` whose default
    /// provider has its model (`HiveAiConfig::effective`).
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let file: InstallationFile = serde_json::from_slice(bytes)
            .map_err(|err| invalid(format!("it does not parse: {err}")))?;
        if file.schema_version != INSTALLATION_SCHEMA_VERSION {
            return Err(invalid(format!(
                "schema_version {} (this release reads {INSTALLATION_SCHEMA_VERSION})",
                file.schema_version
            )));
        }
        if file.version == 0 {
            return Err(invalid("version 0 (the first is 1)".to_string()));
        }
        if let Some(ai) = &file.ai {
            ai.effective()
                .map_err(|err| invalid(format!("its ai: {err}")))?;
        }
        Ok(file)
    }

    /// The bytes the motherbee writes for it: pretty JSON and one newline, so a hand edit shows
    /// as a change of hash.
    pub fn render(&self) -> Result<Vec<u8>> {
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

/// The installation file at `path`: None while the hive's dist does not hold one. A file that is
/// there and is not a valid one is an error.
pub fn read_installation_file(path: &Path) -> Result<Option<InstalledSettings>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(invalid(format!("'{}' unreadable: {err}", path.display()))),
    };
    let file = InstallationFile::parse(&bytes)?;
    Ok(Some(InstalledSettings {
        file,
        sha256: sha256_hex(&bytes),
    }))
}

/// Where a reader's `ai` came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiSettingsSource {
    /// The installation file, at this version and hash.
    Installation { version: u64, sha256: String },
    /// The hive's own hive.yaml: its dist holds no installation file yet.
    HiveYaml,
}

/// A reader's `ai`, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSettings {
    pub ai: Option<HiveAiConfig>,
    pub source: AiSettingsSource,
}

/// A reader's `ai`: the installation file's once the hive holds one, its own hive.yaml's
/// (`hive_yaml_ai`) until then. A file that is there and is not a valid one is an error: no reader
/// falls back from a file it cannot read to a hive.yaml that may be older.
pub fn ai_settings(hive_yaml_ai: Option<&HiveAiConfig>) -> Result<AiSettings> {
    ai_settings_at(Path::new(INSTALLATION_FILE_PATH), hive_yaml_ai)
}

/// `ai_settings`, with the installation file at `path`.
pub fn ai_settings_at(path: &Path, hive_yaml_ai: Option<&HiveAiConfig>) -> Result<AiSettings> {
    Ok(match read_installation_file(path)? {
        Some(installed) => AiSettings {
            ai: installed.file.ai,
            source: AiSettingsSource::Installation {
                version: installed.file.version,
                sha256: installed.sha256,
            },
        },
        None => AiSettings {
            ai: hive_yaml_ai.cloned(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{AiProvider, AiProviderConfigs, AiProviderModelConfig};

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

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fbinst-{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("installation.json")
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
    fn a_file_no_reader_can_use_is_refused() {
        let refused = [
            r#"{"schema_version":2,"version":1}"#,
            r#"{"schema_version":1,"version":0}"#,
            r#"{"schema_version":1}"#,
            r#"{"schema_version":1,"version":1,"ai":{"default_provider":"anthropic","providers":{"openai":{"model":"gpt-5.5"}}}}"#,
            r#"{"schema_version":1,"version":1,"ai":{"default_provider":"openai","providers":{"openai":{"model":"  "}}}}"#,
            "not json",
        ];
        for raw in refused {
            let err = InstallationFile::parse(raw.as_bytes()).expect_err(raw);
            assert!(err.to_string().contains("installation file"), "{err}");
        }
    }

    #[test]
    fn a_reader_takes_the_installation_file_once_its_hive_holds_one() {
        let path = temp_path("settings");
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
        let before = ai_settings_at(&path, Some(&hive_yaml)).expect("no file");
        assert_eq!(before.source, AiSettingsSource::HiveYaml);
        assert_eq!(before.ai.as_ref(), Some(&hive_yaml));
        assert_eq!(
            ai_settings_at(&path, None).expect("no file").ai,
            None,
            "a hive.yaml with no ai"
        );
        // The file: its ai, its version and the hash of its bytes, whatever hive.yaml says.
        let file = InstallationFile {
            schema_version: INSTALLATION_SCHEMA_VERSION,
            version: 4,
            ai: Some(both_providers()),
        };
        let bytes = file.render().expect("renders");
        std::fs::write(&path, &bytes).expect("written");
        let after = ai_settings_at(&path, Some(&hive_yaml)).expect("the file");
        assert_eq!(after.ai, Some(both_providers()));
        assert_eq!(
            after.source,
            AiSettingsSource::Installation {
                version: 4,
                sha256: sha256_hex(&bytes),
            }
        );
        // A file that is there and is not a valid one is an error, not hive.yaml's ai.
        std::fs::write(&path, b"{\"schema_version\":1,\"version\":0}").expect("edited");
        assert!(ai_settings_at(&path, Some(&hive_yaml)).is_err());
        let _ = std::fs::remove_dir_all(path.parent().expect("its dir"));
    }

    #[test]
    fn the_hash_is_the_sha256_of_the_bytes_in_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
