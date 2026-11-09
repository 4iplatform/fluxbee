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
    ai_settings_at, installation_file_path, installation_file_state, sha256_hex, AiProvider,
    AiProviderConfigs, AiProviderModelConfig, AiSdkError, AiSettings, AiSettingsSource,
    EffectiveAiEngine, HiveAiConfig, InstallationFile, InstallationFileState,
    INSTALLATION_SCHEMA_VERSION,
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
    engine_of(settings.ai.as_ref())
}

/// The engine `ai` gives a reader that runs one (`reader_engine`): what a config update compares to
/// decide which readers to restart (docs/hive-config-update-v1.md §4, stage 3c).
pub fn engine_of(ai: Option<&HiveAiConfig>) -> fluxbee_ai_sdk::Result<EffectiveAiEngine> {
    Ok(ai
        .map(HiveAiConfig::effective)
        .transpose()?
        .unwrap_or_else(HiveAiConfig::fallback))
}

/// The values every reader runs on with `ai`: `ai` itself, or for none the fallback engine written
/// out, its provider with its model and no other provider. Every reader runs the same on both
/// (ai.generic's model for another provider is none either way), so a version's record can always
/// name the values it holds (docs/hive-config-update-v1.md §4, stage 3c).
pub fn ai_values(ai: Option<&HiveAiConfig>) -> HiveAiConfig {
    ai.cloned().unwrap_or_else(|| {
        let engine = HiveAiConfig::fallback();
        let model = Some(AiProviderModelConfig {
            model: engine.model,
        });
        let providers = match engine.provider {
            AiProvider::OpenAi => AiProviderConfigs {
                openai: model,
                anthropic: None,
            },
            AiProvider::Anthropic => AiProviderConfigs {
                openai: None,
                anthropic: model,
            },
        };
        HiveAiConfig {
            default_provider: engine.provider,
            providers,
        }
    })
}

/// The providers `ai` gives a model, in a fixed order; the fallback engine's alone without an `ai`.
pub fn providers_with_a_model(ai: Option<&HiveAiConfig>) -> Vec<AiProvider> {
    let values = ai_values(ai);
    [
        (AiProvider::OpenAi, &values.providers.openai),
        (AiProvider::Anthropic, &values.providers.anthropic),
    ]
    .into_iter()
    .filter(|(_, config)| {
        config
            .as_ref()
            .is_some_and(|config| !config.model.trim().is_empty())
    })
    .map(|(provider, _)| provider)
    .collect()
}

/// The operator's `ai`, as SY.admin's `set_installation_ai` takes it and the motherbee's
/// orchestrator writes it (docs/hive-config-update-v1.md §4, stage 3c): exactly `default_provider`
/// and `providers`, each provider named with its `model` and nothing else, so a field no reader
/// reads is refused rather than dropped; and values every reader can use
/// (`InstallationFile::validate`). A provider given as null is not listed, as `get_installation`
/// shows one. The reason is in plain words.
pub fn parse_operator_ai(value: &serde_json::Value) -> Result<HiveAiConfig, String> {
    let fields = value.as_object().ok_or(
        "ai must be an object: {\"default_provider\": \"openai|anthropic\", \"providers\": \
         {\"<provider>\": {\"model\": \"<model>\"}}}",
    )?;
    if let Some(other) = fields
        .keys()
        .find(|key| !matches!(key.as_str(), "default_provider" | "providers"))
    {
        return Err(format!(
            "ai.{other} is not an ai setting: ai takes default_provider and providers"
        ));
    }
    let default_provider = fields
        .get("default_provider")
        .and_then(serde_json::Value::as_str)
        .ok_or("ai.default_provider is required: openai or anthropic")?
        .parse::<AiProvider>()
        .map_err(|err| format!("ai.default_provider: {}", plain_error(&err)))?;
    let listed = fields
        .get("providers")
        .and_then(serde_json::Value::as_object)
        .ok_or(
            "ai.providers is required: an object with the model of each provider the installation \
             lists, {\"<provider>\": {\"model\": \"<model>\"}}",
        )?;
    let mut providers = AiProviderConfigs::default();
    for (name, provider) in listed.iter().filter(|(_, provider)| !provider.is_null()) {
        let parsed = name
            .parse::<AiProvider>()
            .map_err(|_| format!("ai.providers.{name} is not a provider: openai or anthropic"))?;
        let entry = provider
            .as_object()
            .ok_or_else(|| format!("ai.providers.{name} must be an object: {{\"model\": ...}}"))?;
        if let Some(other) = entry.keys().find(|key| key.as_str() != "model") {
            return Err(format!(
                "ai.providers.{name}.{other} is not a provider setting: a provider takes model"
            ));
        }
        let model = entry
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| {
                format!("ai.providers.{name}.model is required and must not be empty")
            })?;
        let slot = match parsed {
            AiProvider::OpenAi => &mut providers.openai,
            AiProvider::Anthropic => &mut providers.anthropic,
        };
        if slot.is_some() {
            return Err(format!("ai.providers names {parsed} twice"));
        }
        *slot = Some(AiProviderModelConfig {
            model: model.to_string(),
        });
    }
    let ai = HiveAiConfig {
        default_provider,
        providers,
    };
    InstallationFile {
        schema_version: INSTALLATION_SCHEMA_VERSION,
        version: 1,
        ai: Some(ai.clone()),
    }
    .validate()
    .map_err(|err| plain_error(&err))?;
    Ok(ai)
}

/// The sha256 of the installation file's bytes at `file`, None while the hive holds none: what a
/// reader built at its start compares before each use (`InstallationBuiltFrom`). A file that cannot
/// be read stamps as its error, so a reader tries again once it can.
pub fn installation_file_stamp(file: &Path) -> Option<String> {
    match std::fs::read(file) {
        Ok(bytes) => Some(sha256_hex(&bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => Some(format!("unreadable: {err}")),
    }
}

/// The installation file a reader's AI runtime was built from (docs/hive-config-update-v1.md §4,
/// stage 3c). SY.admin's executor and Archi take the installation's `ai` when they build their
/// runtime, and no config update restarts them: either may be serving the very action that wrote
/// the new version. Before each use they compare the file with the one their runtime was built
/// from, and look again only when it changed: one small file read and hashed. A build records its
/// stamp only once it reached a verdict (a runtime, no key, or settings no reader can use): a build
/// whose vault did not answer records none, so the next use tries again.
#[derive(Debug, Default)]
pub struct InstallationBuiltFrom {
    /// None while no build reached a verdict; then the file's stamp at the last one
    /// (`installation_file_stamp`).
    stamp: std::sync::Mutex<Option<Option<String>>>,
}

impl InstallationBuiltFrom {
    /// Records the stamp a build took before it read the file: a file that changes while the build
    /// reads it differs from the stamp, and is taken again at the next use.
    pub fn record(&self, stamp: Option<String>) {
        *self
            .stamp
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(stamp);
    }

    /// Whether the next use should look at `file` again: no build reached a verdict yet, or the file
    /// is not the one the last verdict was reached on.
    pub fn changed(&self, file: &Path) -> bool {
        let built = self
            .stamp
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        built.is_none_or(|built| built != installation_file_stamp(file))
    }
}

/// What the vault answered for a reader's provider key (stage 3c's review, F1).
#[derive(Clone, PartialEq, Eq)]
pub enum ProviderKey {
    /// The key.
    Found(String),
    /// The vault answered and gave no key: it holds none for the reader, a value without one, or a
    /// refusal. The reader runs without AI, with the reason, as at a start.
    Missing(String),
    /// The vault did not answer (`fluxbee_sdk::vault_failure_is_transport`): the reader keeps what
    /// it runs, and its next use tries again.
    Unanswered(String),
}

// By hand, so that no `{:?}` of it ever prints a key.
impl std::fmt::Debug for ProviderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderKey::Found(_) => f.write_str("Found(<redacted>)"),
            ProviderKey::Missing(reason) => f.debug_tuple("Missing").field(reason).finish(),
            ProviderKey::Unanswered(reason) => f.debug_tuple("Unanswered").field(reason).finish(),
        }
    }
}

/// A reader's key from what `VaultClient::resolve_resource` answered: a vault that did not answer
/// is told apart from one that answered without a key.
pub fn provider_key(
    answer: Result<Option<serde_json::Value>, fluxbee_sdk::VaultError>,
) -> ProviderKey {
    match answer {
        Ok(Some(value)) => match api_key_of(&value) {
            Some(key) => ProviderKey::Found(key),
            None => ProviderKey::Missing(
                "the vault's secret for this provider holds no api_key".to_string(),
            ),
        },
        Ok(None) => ProviderKey::Missing(
            "the vault holds no key of this provider for it (its own, one assigned to it, or its \
             tenant's general one)"
                .to_string(),
        ),
        Err(err) if fluxbee_sdk::vault_failure_is_transport(&err) => {
            ProviderKey::Unanswered(err.to_string())
        }
        Err(err) => ProviderKey::Missing(err.to_string()),
    }
}

/// The AI key in a vault value: a bare string, or `{"api_key": ...}`.
pub fn api_key_of(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get("api_key").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

/// Where a reader's `ai` comes from now, for the line it logs once it takes a new installation
/// file: the file's version and hash, hive.yaml while the hive holds none, or why the file cannot
/// be used.
pub fn installation_file_label(file: &Path) -> String {
    match installation_file_state(file) {
        Ok(InstallationFileState::Valid(installed)) => {
            ai_source_label(&AiSettingsSource::Installation {
                version: installed.file.version,
                sha256: installed.sha256,
            })
        }
        Ok(InstallationFileState::Absent) => ai_source_label(&AiSettingsSource::HiveYaml),
        Ok(InstallationFileState::Invalid { error, .. }) => {
            format!("an installation file no reader can use ({error})")
        }
        Err(err) => plain_error(&err),
    }
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

    /// docs/hive-config-update-v1.md §4, stage 3c: the operator's `ai` is taken exactly as every
    /// reader reads it, a field no reader reads refused rather than dropped, and values every reader
    /// can use; the reason is in plain words.
    #[test]
    fn the_operator_s_ai_is_taken_exactly_and_with_every_reader_s_rules() {
        let ai = parse_operator_ai(&serde_json::json!({
            "default_provider": "anthropic",
            "providers": {
                "openai": {"model": " gpt-5.5 "},
                "anthropic": {"model": "claude-sonnet-5-5"},
            },
        }))
        .expect("both providers");
        assert_eq!(ai, both_providers(), "models are trimmed");
        // The values as get_installation shows them, a provider not listed as null, go back as
        // they are.
        let shown = serde_json::to_value(ai_values(None)).expect("json");
        assert!(shown["providers"]["anthropic"].is_null(), "{shown}");
        assert_eq!(
            parse_operator_ai(&shown).expect("as shown"),
            ai_values(None)
        );

        for (value, reason) in [
            (serde_json::json!("anthropic"), "ai must be an object"),
            (
                serde_json::json!({"default_provider": "openai", "providers": {"openai": {"model": "x"}}, "temperature": 1}),
                "ai.temperature is not an ai setting",
            ),
            (
                serde_json::json!({"providers": {"openai": {"model": "x"}}}),
                "ai.default_provider is required",
            ),
            (
                serde_json::json!({"default_provider": "gemini", "providers": {"openai": {"model": "x"}}}),
                "ai.default_provider: unsupported AI provider 'gemini'",
            ),
            (
                serde_json::json!({"default_provider": "openai"}),
                "ai.providers is required",
            ),
            (
                serde_json::json!({"default_provider": "openai", "providers": {"gemini": {"model": "x"}}}),
                "ai.providers.gemini is not a provider",
            ),
            (
                serde_json::json!({"default_provider": "openai", "providers": {"openai": {"model": "x", "api_key": "k"}}}),
                "ai.providers.openai.api_key is not a provider setting",
            ),
            (
                serde_json::json!({"default_provider": "openai", "providers": {"openai": {"model": " "}}}),
                "ai.providers.openai.model is required and must not be empty",
            ),
            (
                serde_json::json!({"default_provider": "openai", "providers": {"openai": {"model": "x"}, "OpenAI": {"model": "y"}}}),
                "ai.providers names openai twice",
            ),
            (
                serde_json::json!({"default_provider": "anthropic", "providers": {"openai": {"model": "x"}}}),
                "ai.providers.anthropic is required",
            ),
        ] {
            let err = parse_operator_ai(&value).expect_err(reason);
            assert!(err.contains(reason), "{err} (expected {reason})");
            assert!(!err.contains("protocol error"), "{err}");
        }
    }

    /// A version's values, and the providers it gives a model: an `ai` as it is, and none the
    /// fallback engine written out, on which every reader runs the same.
    #[test]
    fn a_version_without_ai_holds_the_fallback_engine_s_values() {
        let fallback = ai_values(None);
        assert_eq!(
            engine_of(Some(&fallback)).expect("engine"),
            engine_of(None).expect("fallback")
        );
        assert_eq!(fallback.default_provider, AiProvider::OpenAi);
        assert!(fallback.providers.anthropic.is_none());
        assert_eq!(ai_values(Some(&both_providers())), both_providers());
        assert_eq!(providers_with_a_model(None), vec![AiProvider::OpenAi]);
        assert_eq!(
            providers_with_a_model(Some(&both_providers())),
            vec![AiProvider::OpenAi, AiProvider::Anthropic]
        );
        assert_eq!(
            engine_of(Some(&both_providers())).expect("engine"),
            EffectiveAiEngine {
                provider: AiProvider::Anthropic,
                model: "claude-sonnet-5-5".to_string(),
            }
        );
    }

    /// SY.admin's executor and Archi take a new installation file at their next use: the stamp of
    /// the file their runtime was built from against the file now, nothing before a build recorded
    /// one.
    #[test]
    fn a_reader_built_from_one_file_sees_the_next_one() {
        let dist = temp_dist("built-from");
        let file = installation_file_path(&dist);
        let built = InstallationBuiltFrom::default();
        // No build reached a verdict yet (its vault did not answer): the next use looks again.
        assert!(built.changed(&file), "no verdict yet");
        built.record(installation_file_stamp(&file));
        assert!(!built.changed(&file), "built without a file, and none yet");
        assert_eq!(
            installation_file_label(&file),
            "hive.yaml: this hive holds no installation file yet"
        );

        let version = |version: u64, ai: HiveAiConfig| {
            InstallationFile {
                schema_version: INSTALLATION_SCHEMA_VERSION,
                version,
                ai: Some(ai),
            }
            .render()
            .expect("renders")
        };
        let first = version(1, both_providers());
        std::fs::write(&file, &first).expect("version 1");
        assert!(built.changed(&file), "the file arrived");
        built.record(installation_file_stamp(&file));
        assert!(!built.changed(&file));
        assert!(installation_file_label(&file).starts_with("installation file v1 (sha256 "));

        let mut openai = both_providers();
        openai.default_provider = AiProvider::OpenAi;
        std::fs::write(&file, version(2, openai)).expect("version 2");
        assert!(built.changed(&file), "a new version");
        assert!(installation_file_label(&file).starts_with("installation file v2 (sha256 "));

        std::fs::write(&file, b"{\"schema_version\":1,\"version\":0}").expect("edited");
        assert!(installation_file_label(&file).contains("no reader can use"));
        let _ = std::fs::remove_dir_all(&dist);
    }

    /// A vault that did not answer is told apart from one that holds no key: the first keeps what a
    /// reader runs, the second takes it down with the reason (stage 3c's review, F1). No `{:?}`
    /// prints a key.
    #[test]
    fn a_vault_that_did_not_answer_is_not_a_missing_key() {
        let found = provider_key(Ok(Some(serde_json::json!({"api_key": " sk-1 "}))));
        assert_eq!(found, ProviderKey::Found("sk-1".to_string()));
        assert_eq!(format!("{found:?}"), "Found(<redacted>)");
        assert_eq!(
            provider_key(Ok(Some(serde_json::json!("sk-2")))),
            ProviderKey::Found("sk-2".to_string())
        );
        for answer in [
            Ok(None),
            Ok(Some(serde_json::json!({"token": "x"}))),
            Err(fluxbee_sdk::VaultError::Service {
                code: "FORBIDDEN".to_string(),
                message: "not this tenant's".to_string(),
            }),
        ] {
            assert!(
                matches!(provider_key(answer), ProviderKey::Missing(_)),
                "a verdict"
            );
        }
        for unanswered in [
            fluxbee_sdk::VaultError::Unreachable {
                reason: "NODE_NOT_FOUND".to_string(),
                original_dst: "SY.vault@motherbee".to_string(),
            },
            fluxbee_sdk::VaultError::ActionTimeout {
                action: "VAULT_LIST".to_string(),
                trace_id: "t".to_string(),
                target: "SY.vault@motherbee".to_string(),
                timeout_ms: 5_000,
            },
        ] {
            assert!(matches!(
                provider_key(Err(unanswered)),
                ProviderKey::Unanswered(_)
            ));
        }
    }
}
