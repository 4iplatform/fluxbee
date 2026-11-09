//! A reader's `ai` on its hive (docs/hive-config-update-v1.md §4): the installation file's once the
//! hive's dist holds one, and the hive's own hive.yaml's until then (`fluxbee_ai_sdk::ai_settings_at`).
//! The readers of this crate (SY.admin, SY.architect, SY.cognition) and the orchestrator, which
//! writes the file on the motherbee, take it from here; ai.generic and SY.frontdesk.gov, in crates
//! of their own, do the same with the SDK.
//!
//! A reader's hive.yaml struct takes `ai` untyped (`serde_yaml::Value`): it is typed only when the
//! reader falls back to it, so an `ai` no reader uses any more never stops one that has the file.

use std::path::{Path, PathBuf};

use fluxbee_ai_sdk::{
    ai_settings_at, installation_file_path, AiSdkError, AiSettings, AiSettingsSource,
    EffectiveAiEngine, HiveAiConfig,
};
use serde::Deserialize;

/// The dist root of a hive whose hive.yaml has no `dist.path`.
pub const DEFAULT_DIST_ROOT: &str = "/var/lib/fluxbee/dist";

/// hive.yaml's `dist:` section, as far as a reader needs it: where the hive's dist is.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DistPathSection {
    #[serde(default)]
    pub path: Option<String>,
}

/// The hive's dist root: hive.yaml's `dist.path`, or the default.
pub fn dist_root(dist: Option<&DistPathSection>) -> PathBuf {
    dist.and_then(|dist| dist.path.as_deref())
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_DIST_ROOT), PathBuf::from)
}

/// The installation file of the hive whose hive.yaml has this `dist:` section.
pub fn installation_file(dist: Option<&DistPathSection>) -> PathBuf {
    installation_file_path(&dist_root(dist))
}

/// hive.yaml's `ai` section as the readers take it: None without one. Typed here, only when a
/// reader falls back to it.
pub fn hive_yaml_ai(
    section: Option<&serde_yaml::Value>,
) -> fluxbee_ai_sdk::Result<Option<HiveAiConfig>> {
    section
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_yaml::from_value::<HiveAiConfig>(value.clone())
                .map_err(|err| AiSdkError::Protocol(format!("hive.yaml's ai: {err}")))
        })
        .transpose()
}

/// A reader's `ai`: the installation file's at `file` once its hive holds one, else hive.yaml's
/// `section`. A file that is there and that no reader can use is an error, never a fallback.
pub fn reader_ai_settings_at(
    file: &Path,
    section: Option<&serde_yaml::Value>,
) -> fluxbee_ai_sdk::Result<AiSettings> {
    ai_settings_at(file, || hive_yaml_ai(section))
}

/// A reader's `ai`, from the hive.yaml sections it parsed: its `dist:` (where the file is) and its
/// untyped `ai`.
pub fn reader_ai_settings(
    dist: Option<&DistPathSection>,
    section: Option<&serde_yaml::Value>,
) -> fluxbee_ai_sdk::Result<AiSettings> {
    reader_ai_settings_at(&installation_file(dist), section)
}

/// What a reader that runs one engine runs on: its `ai`'s default provider with that provider's
/// model, or the fallback engine without an `ai` (`HiveAiConfig::fallback`).
pub fn reader_engine(settings: &AiSettings) -> fluxbee_ai_sdk::Result<EffectiveAiEngine> {
    Ok(settings
        .ai
        .as_ref()
        .map(HiveAiConfig::effective)
        .transpose()?
        .unwrap_or_else(HiveAiConfig::fallback))
}

/// Where a reader's `ai` came from, for the line it logs: the installation file's version and
/// hash, or hive.yaml while its hive holds no file.
pub fn ai_source_label(source: &AiSettingsSource) -> String {
    match source {
        AiSettingsSource::Installation { version, sha256 } => {
            format!(
                "installation file v{version} (sha256 {})",
                short_hash(sha256)
            )
        }
        AiSettingsSource::HiveYaml => {
            "hive.yaml: this hive holds no installation file yet".to_string()
        }
    }
}

/// The same, for a status or config payload: `{from: "installation_file", version, sha256}` or
/// `{from: "hive_yaml"}`.
pub fn ai_source_json(source: &AiSettingsSource) -> serde_json::Value {
    match source {
        AiSettingsSource::Installation { version, sha256 } => serde_json::json!({
            "from": "installation_file",
            "version": version,
            "sha256": sha256,
        }),
        AiSettingsSource::HiveYaml => serde_json::json!({ "from": "hive_yaml" }),
    }
}

/// An error's own words, without the "protocol error:" the SDK's Display adds.
pub fn plain_error(err: &AiSdkError) -> String {
    match err {
        AiSdkError::Protocol(detail) => detail.clone(),
        other => other.to_string(),
    }
}

/// The first 12 hex digits of a sha256, enough to tell versions apart in a log line.
pub fn short_hash(sha256: &str) -> &str {
    sha256.get(..12).unwrap_or(sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_ai_sdk::{
        sha256_hex, AiProvider, AiProviderConfigs, AiProviderModelConfig, InstallationFile,
        INSTALLATION_SCHEMA_VERSION,
    };

    fn temp_dist(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fb-ai-settings-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(dir.join("installation")).expect("temp dist");
        dir
    }

    fn yaml(text: &str) -> serde_yaml::Value {
        serde_yaml::from_str(text).expect("yaml")
    }

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

    #[test]
    fn the_dist_root_is_hive_yaml_s_or_the_default() {
        assert_eq!(dist_root(None), PathBuf::from(DEFAULT_DIST_ROOT));
        let blank = DistPathSection {
            path: Some("  ".to_string()),
        };
        assert_eq!(dist_root(Some(&blank)), PathBuf::from(DEFAULT_DIST_ROOT));
        let own = DistPathSection {
            path: Some(" /srv/dist ".to_string()),
        };
        assert_eq!(
            installation_file(Some(&own)),
            PathBuf::from("/srv/dist/installation/installation.json")
        );
        assert_eq!(
            installation_file(None),
            PathBuf::from(fluxbee_ai_sdk::INSTALLATION_FILE_PATH)
        );
    }

    /// A hive without the file reads its hive.yaml, as before: its `ai` typed now, an invalid one
    /// an error, none the fallback engine.
    #[test]
    fn without_the_file_a_reader_takes_hive_yaml_s_ai() {
        let dist = temp_dist("fallback");
        let file = installation_file_path(&dist);
        let section = yaml(
            "default_provider: anthropic\nproviders:\n  openai:\n    model: gpt-5.5\n  anthropic:\n    model: claude-sonnet-5-5\n",
        );
        let settings = reader_ai_settings_at(&file, Some(&section)).expect("hive.yaml");
        assert_eq!(settings.source, AiSettingsSource::HiveYaml);
        assert_eq!(settings.ai, Some(both_providers()));
        let engine = reader_engine(&settings).expect("engine");
        assert_eq!(
            (engine.provider, engine.model.as_str()),
            (AiProvider::Anthropic, "claude-sonnet-5-5")
        );
        assert!(ai_source_label(&settings.source).starts_with("hive.yaml"));
        assert_eq!(
            ai_source_json(&settings.source),
            serde_json::json!({"from": "hive_yaml"})
        );

        let none = reader_ai_settings_at(&file, None).expect("no ai");
        assert_eq!(
            reader_engine(&none).expect("fallback"),
            HiveAiConfig::fallback()
        );
        let null = yaml("~");
        assert_eq!(
            reader_ai_settings_at(&file, Some(&null))
                .expect("null ai")
                .ai,
            None
        );

        let unknown = yaml("default_provider: gemini\n");
        let err = reader_ai_settings_at(&file, Some(&unknown)).expect_err("unknown provider");
        assert!(plain_error(&err).contains("hive.yaml's ai"), "{err}");
        let no_model = yaml("default_provider: anthropic\nproviders:\n  openai:\n    model: x\n");
        let settings = reader_ai_settings_at(&file, Some(&no_model)).expect("typed");
        assert!(
            reader_engine(&settings).is_err(),
            "no model for its default"
        );
        let _ = std::fs::remove_dir_all(&dist);
    }

    /// Once the hive holds the file its `ai` is the file's, whatever hive.yaml's says, an invalid
    /// one included; a file no reader can use is an error, never hive.yaml's.
    #[test]
    fn with_the_file_a_reader_takes_the_file_and_never_hive_yaml() {
        let dist = temp_dist("file");
        let file = installation_file_path(&dist);
        let bytes = InstallationFile {
            schema_version: INSTALLATION_SCHEMA_VERSION,
            version: 1,
            ai: Some(both_providers()),
        }
        .render()
        .expect("renders");
        std::fs::write(&file, &bytes).expect("written");
        let old = yaml("default_provider: gemini\nproviders: 7\n");
        let settings = reader_ai_settings_at(&file, Some(&old)).expect("the file");
        assert_eq!(settings.ai, Some(both_providers()));
        assert_eq!(
            settings.source,
            AiSettingsSource::Installation {
                version: 1,
                sha256: sha256_hex(&bytes),
            }
        );
        let label = ai_source_label(&settings.source);
        assert!(
            label.starts_with("installation file v1 (sha256 "),
            "{label}"
        );
        assert!(label.contains(short_hash(&sha256_hex(&bytes))), "{label}");
        assert_eq!(
            ai_source_json(&settings.source)["version"],
            serde_json::json!(1)
        );

        std::fs::write(&file, b"{\"schema_version\":1,\"version\":1,\"ai\":7}").expect("broken");
        let valid = yaml("default_provider: openai\nproviders:\n  openai:\n    model: gpt-5.5\n");
        let err = reader_ai_settings_at(&file, Some(&valid)).expect_err("never hive.yaml's");
        assert!(plain_error(&err).contains("installation file"), "{err}");
        let _ = std::fs::remove_dir_all(&dist);
    }

    #[test]
    fn a_short_hash_is_its_first_twelve_digits() {
        assert_eq!(short_hash("0123456789abcdef"), "0123456789ab");
        assert_eq!(short_hash("abc"), "abc");
    }
}
