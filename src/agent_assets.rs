//! The agent assets (docs/agent-tools-v1.md, docs/digital-executive-v1-stage-2.md §6.2): the
//! role, skill, handbook and personality files an AI node's definition names by hash, in
//! `<blob>/agent-assets/<hash>.json`. Their checks, their canonical bytes and their hash, and
//! their writer, shared by SY.architect, which writes them for the operator, and SY.deploy, which
//! writes a deployment's, so both write the same files.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The error of an asset check or write: the same as SY.architect's, so its callers pass it on.
pub type AgentAssetError = Box<dyn std::error::Error + Send + Sync>;

pub const AGENT_ASSET_MAX_BYTES: usize = 256 * 1024;
pub const AGENT_ASSET_ROLE_MAX_LIMITS: usize = 32;
pub const AGENT_ASSET_SKILL_MAX_INSTRUCTIONS: usize = 64;
pub const AGENT_ASSET_SKILL_MAX_EXAMPLES: usize = 16;
pub const AGENT_ASSET_SKILL_MAX_CONSTRAINTS: usize = 32;
pub const AGENT_ASSET_HANDBOOK_MAX_SECTIONS: usize = 128;

pub fn validate_hash64(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn normalize_agent_asset_hash(hash: &str) -> Result<String, AgentAssetError> {
    let normalized = hash.trim().to_ascii_lowercase();
    if validate_hash64(&normalized) {
        Ok(normalized)
    } else {
        Err("agent asset hash must be a 64-hex sha256 value".into())
    }
}

pub fn canonicalize_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut out = serde_json::Map::new();
            for (key, value) in entries {
                out.insert(key.clone(), canonicalize_json_value(value));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json_value).collect()),
        _ => value.clone(),
    }
}

pub fn canonical_agent_asset_bytes(asset: &Value) -> Result<Vec<u8>, AgentAssetError> {
    let canonical = canonicalize_json_value(asset);
    Ok(serde_json::to_vec(&canonical)?)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn clean_required_string(value: &Value, field: &str) -> Result<String, AgentAssetError> {
    let Some(raw) = value.as_str() else {
        return Err(format!("{field} must be a string").into());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Err(format!("{field} is required").into())
    } else {
        Ok(trimmed.to_string())
    }
}

pub fn clean_optional_string(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<String>, AgentAssetError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(raw) = value.as_str() else {
        return Err(format!("{field} must be a string").into());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

pub fn clean_string_array(
    value: Option<&Value>,
    field: &str,
    max_items: usize,
) -> Result<Vec<String>, AgentAssetError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(format!("{field} must be an array of strings").into());
    };
    if items.len() > max_items {
        return Err(format!("{field} max items is {max_items}").into());
    }
    let mut out = Vec::new();
    for item in items {
        let Some(raw) = item.as_str() else {
            return Err(format!("{field} must contain only strings").into());
        };
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

pub fn build_role_asset_value(arguments: &Value) -> Result<Value, AgentAssetError> {
    let name = clean_required_string(arguments.get("name").unwrap_or(&Value::Null), "role.name")?;
    let description = clean_required_string(
        arguments.get("description").unwrap_or(&Value::Null),
        "role.description",
    )?;
    let tone = clean_optional_string(arguments.get("tone"), "role.tone")?;
    let limits = clean_string_array(
        arguments.get("limits"),
        "role.limits",
        AGENT_ASSET_ROLE_MAX_LIMITS,
    )?;
    let mut map = serde_json::Map::new();
    map.insert("asset_type".to_string(), json!("role"));
    map.insert("name".to_string(), json!(name));
    map.insert("description".to_string(), json!(description));
    if let Some(tone) = tone {
        map.insert("tone".to_string(), json!(tone));
    }
    if !limits.is_empty() {
        map.insert("limits".to_string(), json!(limits));
    }
    Ok(Value::Object(map))
}

pub fn build_skill_examples(value: Option<&Value>) -> Result<Vec<Value>, AgentAssetError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err("skill.examples must be an array".into());
    };
    if items.len() > AGENT_ASSET_SKILL_MAX_EXAMPLES {
        return Err(format!(
            "skill.examples max items is {}",
            AGENT_ASSET_SKILL_MAX_EXAMPLES
        )
        .into());
    }
    let mut out = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else {
            return Err("skill.examples must contain objects".into());
        };
        let input =
            clean_optional_string(obj.get("input"), "skill.examples.input")?.unwrap_or_default();
        let output =
            clean_optional_string(obj.get("output"), "skill.examples.output")?.unwrap_or_default();
        if input.is_empty() && output.is_empty() {
            continue;
        }
        out.push(json!({"input": input, "output": output}));
    }
    Ok(out)
}

pub fn build_skill_asset_value(arguments: &Value) -> Result<Value, AgentAssetError> {
    let name = clean_required_string(arguments.get("name").unwrap_or(&Value::Null), "skill.name")?;
    let description = clean_optional_string(arguments.get("description"), "skill.description")?;
    let instructions = clean_string_array(
        arguments.get("instructions"),
        "skill.instructions",
        AGENT_ASSET_SKILL_MAX_INSTRUCTIONS,
    )?;
    if instructions.is_empty() {
        return Err("skill.instructions must contain at least one item".into());
    }
    let constraints = clean_string_array(
        arguments.get("constraints"),
        "skill.constraints",
        AGENT_ASSET_SKILL_MAX_CONSTRAINTS,
    )?;
    let examples = build_skill_examples(arguments.get("examples"))?;
    let mut map = serde_json::Map::new();
    map.insert("asset_type".to_string(), json!("skill"));
    map.insert("name".to_string(), json!(name));
    if let Some(description) = description {
        map.insert("description".to_string(), json!(description));
    }
    map.insert("instructions".to_string(), json!(instructions));
    if !constraints.is_empty() {
        map.insert("constraints".to_string(), json!(constraints));
    }
    if !examples.is_empty() {
        map.insert("examples".to_string(), Value::Array(examples));
    }
    Ok(Value::Object(map))
}

pub fn count_handbook_sections(sections: &[Value]) -> usize {
    sections
        .iter()
        .map(|section| {
            1 + section
                .get("subsections")
                .and_then(Value::as_array)
                .map(|children| count_handbook_sections(children))
                .unwrap_or(0)
        })
        .sum()
}

pub fn build_handbook_sections(value: Option<&Value>) -> Result<Vec<Value>, AgentAssetError> {
    let Some(value) = value else {
        return Err("handbook.sections is required".into());
    };
    let Some(items) = value.as_array() else {
        return Err("handbook.sections must be an array".into());
    };
    if items.is_empty() {
        return Err("handbook.sections must contain at least one item".into());
    }
    let mut out = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else {
            return Err("handbook.sections must contain objects".into());
        };
        let title =
            clean_optional_string(obj.get("title"), "handbook.sections.title")?.unwrap_or_default();
        let content = clean_optional_string(obj.get("content"), "handbook.sections.content")?
            .unwrap_or_default();
        let subsections = if obj.contains_key("subsections") {
            build_handbook_sections(obj.get("subsections"))?
        } else {
            Vec::new()
        };
        if title.is_empty() && content.is_empty() && subsections.is_empty() {
            return Err("each handbook section must contain title, content, or subsections".into());
        }
        let mut section = serde_json::Map::new();
        if !title.is_empty() {
            section.insert("title".to_string(), json!(title));
        }
        if !content.is_empty() {
            section.insert("content".to_string(), json!(content));
        }
        if !subsections.is_empty() {
            section.insert("subsections".to_string(), Value::Array(subsections));
        }
        out.push(Value::Object(section));
    }
    Ok(out)
}

pub fn build_handbook_asset_value(arguments: &Value) -> Result<Value, AgentAssetError> {
    let name = clean_required_string(
        arguments.get("name").unwrap_or(&Value::Null),
        "handbook.name",
    )?;
    let sections = build_handbook_sections(arguments.get("sections"))?;
    let section_count = count_handbook_sections(&sections);
    if section_count > AGENT_ASSET_HANDBOOK_MAX_SECTIONS {
        return Err(format!(
            "handbook.sections max recursive count is {}",
            AGENT_ASSET_HANDBOOK_MAX_SECTIONS
        )
        .into());
    }
    Ok(json!({
        "asset_type": "handbook",
        "name": name,
        "sections": sections,
    }))
}

pub fn build_personality_asset_value(arguments: &Value) -> Result<Value, AgentAssetError> {
    let name = clean_required_string(
        arguments.get("name").unwrap_or(&Value::Null),
        "personality.name",
    )?;
    let description =
        clean_optional_string(arguments.get("description"), "personality.description")?;
    let system_fields_obj = arguments
        .get("system_fields")
        .and_then(Value::as_object)
        .ok_or_else(|| "personality.system_fields is required".to_string())?;
    let timezone = clean_required_string(
        system_fields_obj.get("timezone").unwrap_or(&Value::Null),
        "personality.system_fields.timezone",
    )?;
    let country_code = clean_required_string(
        system_fields_obj
            .get("country_code")
            .unwrap_or(&Value::Null),
        "personality.system_fields.country_code",
    )?;
    let primary_language = clean_required_string(
        system_fields_obj
            .get("primary_language")
            .unwrap_or(&Value::Null),
        "personality.system_fields.primary_language",
    )?;
    let region_code = clean_optional_string(
        system_fields_obj.get("region_code"),
        "personality.system_fields.region_code",
    )?;
    let additional_languages = system_fields_obj
        .get("additional_languages")
        .cloned()
        .unwrap_or(Value::Null);
    let mut system_fields = serde_json::Map::new();
    system_fields.insert("timezone".to_string(), json!(timezone));
    system_fields.insert("country_code".to_string(), json!(country_code));
    if let Some(region) = region_code {
        system_fields.insert("region_code".to_string(), json!(region));
    }
    system_fields.insert("primary_language".to_string(), json!(primary_language));
    if let Some(items) = additional_languages.as_array() {
        if !items.is_empty() {
            system_fields.insert(
                "additional_languages".to_string(),
                Value::Array(items.clone()),
            );
        }
    }

    let mut map = serde_json::Map::new();
    map.insert("asset_type".to_string(), json!("personality"));
    map.insert("name".to_string(), json!(name));
    if let Some(desc) = description {
        map.insert("description".to_string(), json!(desc));
    }
    map.insert("system_fields".to_string(), Value::Object(system_fields));
    if let Some(bio) = arguments.get("biographical").filter(|v| v.is_object()) {
        map.insert("biographical".to_string(), bio.clone());
    }
    if let Some(nar) = arguments.get("narrative").filter(|v| v.is_object()) {
        map.insert("narrative".to_string(), nar.clone());
    }
    if let Some(ext) = arguments.get("extensions").filter(|v| v.is_object()) {
        if !ext.as_object().map(|m| m.is_empty()).unwrap_or(true) {
            map.insert("extensions".to_string(), ext.clone());
        }
    }
    Ok(Value::Object(map))
}

pub fn validate_agent_asset_value(asset: &Value) -> Result<(String, String), AgentAssetError> {
    let obj = asset
        .as_object()
        .ok_or_else(|| "agent asset must be a JSON object".to_string())?;
    let asset_type = asset
        .get("asset_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let name = asset
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "agent asset name is required".to_string())?;
    match asset_type {
        "role" => {
            ensure_allowed_asset_fields(
                obj,
                &["asset_type", "name", "description", "tone", "limits"],
                "role",
            )?;
            build_role_asset_value(asset)?;
        }
        "skill" => {
            ensure_allowed_asset_fields(
                obj,
                &[
                    "asset_type",
                    "name",
                    "description",
                    "instructions",
                    "examples",
                    "constraints",
                ],
                "skill",
            )?;
            build_skill_asset_value(asset)?;
        }
        "handbook" => {
            ensure_allowed_asset_fields(obj, &["asset_type", "name", "sections"], "handbook")?;
            build_handbook_asset_value(asset)?;
        }
        "personality" => {
            ensure_allowed_asset_fields(
                obj,
                &[
                    "asset_type",
                    "name",
                    "description",
                    "system_fields",
                    "biographical",
                    "narrative",
                    "extensions",
                ],
                "personality",
            )?;
            build_personality_asset_value(asset)?;
        }
        _ => return Err(format!("unsupported agent asset_type '{asset_type}'").into()),
    }
    Ok((asset_type.to_string(), name.to_string()))
}

pub fn ensure_allowed_asset_fields(
    obj: &serde_json::Map<String, Value>,
    allowed: &[&str],
    asset_type: &str,
) -> Result<(), AgentAssetError> {
    for key in obj.keys() {
        if !allowed.iter().any(|allowed_key| key == allowed_key) {
            return Err(format!("{asset_type} asset has unsupported field '{key}'").into());
        }
    }
    Ok(())
}

/// An asset checked and ready to write: its type and name, its canonical bytes and their SHA-256,
/// the 64 lowercase hex that name its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedAgentAsset {
    pub asset_type: String,
    pub name: String,
    pub bytes: Vec<u8>,
    pub hash: String,
}

/// Checks `asset` (`validate_agent_asset_value`), and gives its canonical bytes and their hash;
/// one over `AGENT_ASSET_MAX_BYTES` is refused.
pub fn prepare_agent_asset(asset: &Value) -> Result<PreparedAgentAsset, AgentAssetError> {
    let (asset_type, name) = validate_agent_asset_value(asset)?;
    let bytes = canonical_agent_asset_bytes(asset)?;
    if bytes.len() > AGENT_ASSET_MAX_BYTES {
        return Err(format!(
            "agent asset too large: {} bytes > {}",
            bytes.len(),
            AGENT_ASSET_MAX_BYTES
        )
        .into());
    }
    let hash = sha256_hex(&bytes);
    Ok(PreparedAgentAsset {
        asset_type,
        name,
        bytes,
        hash,
    })
}

/// Writes a prepared asset as `<asset_dir>/<hash>.json`, and answers its path and whether it was
/// written now (`false`: the file was there, with the same bytes). A file there with other bytes
/// is refused, never replaced.
pub fn store_prepared_agent_asset(
    asset_dir: &Path,
    prepared: &PreparedAgentAsset,
) -> Result<(PathBuf, bool), AgentAssetError> {
    let hash = &prepared.hash;
    fs::create_dir_all(asset_dir)?;
    let path = asset_dir.join(format!("{hash}.json"));
    let existed = path.exists();
    if existed {
        let existing = fs::read(&path)?;
        let existing_hash = sha256_hex(&existing);
        if existing_hash != *hash {
            return Err(format!(
                "refusing to reuse corrupted agent asset path '{}': expected hash {hash}, got {existing_hash}",
                path.display()
            )
            .into());
        }
    } else {
        // agent-assets/ is shared with the workers by Syncthing (FINDINGS A-72): the file is written
        // next to it, in the blob root (same filesystem, not shared), and renamed in whole, so
        // Syncthing never sees a temp file. Its mode is set here, whatever the umask.
        let blob_root = asset_dir
            .parent()
            .ok_or("agent-assets has no parent directory")?;
        let tmp_path = blob_root.join(format!(".agent-asset-{hash}.tmp"));
        let written = fs::write(&tmp_path, &prepared.bytes)
            .and_then(|()| fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o644)))
            .and_then(|()| fs::rename(&tmp_path, &path));
        if let Err(err) = written {
            let _ = fs::remove_file(&tmp_path);
            return Err(err.into());
        }
    }
    Ok((path, !existed))
}

/// Whether `<asset_dir>/<hash>.json` is there and holds the bytes its name says.
pub fn agent_asset_file_matches(asset_dir: &Path, hash: &str) -> bool {
    let Ok(hash) = normalize_agent_asset_hash(hash) else {
        return false;
    };
    fs::read(asset_dir.join(format!("{hash}.json")))
        .map(|bytes| sha256_hex(&bytes) == hash)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role() -> Value {
        json!({"asset_type": "role", "name": "Desk", "description": "Answers the desk."})
    }

    #[test]
    fn an_asset_is_named_by_the_hash_of_its_canonical_bytes() {
        let prepared = prepare_agent_asset(&role()).expect("a valid role");
        assert_eq!(prepared.asset_type, "role");
        assert_eq!(prepared.name, "Desk");
        assert_eq!(prepared.hash, sha256_hex(&prepared.bytes));
        // The keys sorted, whatever order they came in.
        let reordered: Value = serde_json::from_str(
            r#"{"name": "Desk", "description": "Answers the desk.", "asset_type": "role"}"#,
        )
        .expect("json");
        assert_eq!(
            prepare_agent_asset(&reordered).expect("valid").hash,
            prepared.hash
        );
        assert!(
            prepare_agent_asset(&json!({"asset_type": "role", "name": "x", "tone": 1})).is_err()
        );
    }

    #[test]
    fn a_stored_asset_is_found_and_never_replaced_by_other_bytes() {
        let blob =
            std::env::temp_dir().join(format!("fb-assets-{}", uuid::Uuid::new_v4().simple()));
        let dir = blob.join("agent-assets");
        let prepared = prepare_agent_asset(&role()).expect("valid");
        assert!(!agent_asset_file_matches(&dir, &prepared.hash));
        let (path, written) = store_prepared_agent_asset(&dir, &prepared).expect("written");
        assert!(written);
        assert_eq!(fs::read(&path).expect("read"), prepared.bytes);
        assert!(agent_asset_file_matches(&dir, &prepared.hash));
        let (_, again) = store_prepared_agent_asset(&dir, &prepared).expect("there");
        assert!(!again);
        fs::write(&path, b"{}").expect("corrupt");
        assert!(!agent_asset_file_matches(&dir, &prepared.hash));
        assert!(store_prepared_agent_asset(&dir, &prepared).is_err());
        let _ = fs::remove_dir_all(&blob);
    }
}
