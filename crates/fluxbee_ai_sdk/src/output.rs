use fluxbee_sdk::blob::{BlobRef, BlobToolkit};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{AiSdkError, Result};
use crate::text_payload::{build_text_response_with_options, TextResponseOptions};

const AI_USER_ARTIFACT_FILENAME_MAX_CHARS: usize = 128;
const ALLOWED_USER_ARTIFACT_MIMES: &[(&str, &[&str])] = &[
    ("text/plain", &["txt"]),
    ("text/csv", &["csv"]),
    ("application/json", &["json"]),
    ("text/markdown", &["md", "markdown"]),
    ("text/html", &["html", "htm"]),
    ("application/pdf", &["pdf"]),
    ("image/png", &["png"]),
    ("image/jpeg", &["jpg", "jpeg"]),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        &["xlsx"],
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        &["docx"],
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AiBehaviorOutput {
    Text(String),
    Final(AiFinalOutput),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AiFinalOutput {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<AiUserArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiUserArtifact {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub filename: String,
}

impl AiBehaviorOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text(content.into())
    }

    pub fn final_output(output: AiFinalOutput) -> Self {
        Self::Final(output)
    }
}

impl AiFinalOutput {
    pub fn new(text: Option<String>, artifacts: Vec<AiUserArtifact>) -> Self {
        Self { text, artifacts }
    }

    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            artifacts: Vec::new(),
        }
    }
}

impl AiUserArtifact {
    pub fn new(
        bytes: impl Into<Vec<u8>>,
        mime: impl Into<String>,
        filename: impl Into<String>,
    ) -> Self {
        Self {
            bytes: bytes.into(),
            mime: mime.into(),
            filename: filename.into(),
        }
    }

    pub fn from_text(filename: impl Into<String>, content: impl Into<String>) -> Result<Self> {
        Self::new(content.into().into_bytes(), "text/plain", filename).validated()
    }

    pub fn from_markdown(filename: impl Into<String>, content: impl Into<String>) -> Result<Self> {
        Self::new(content.into().into_bytes(), "text/markdown", filename).validated()
    }

    pub fn from_html(filename: impl Into<String>, content: impl Into<String>) -> Result<Self> {
        Self::new(content.into().into_bytes(), "text/html", filename).validated()
    }

    pub fn from_json<T: Serialize>(filename: impl Into<String>, value: &T) -> Result<Self> {
        let bytes = serde_json::to_vec_pretty(value)?;
        Self::new(bytes, "application/json", filename).validated()
    }

    pub fn from_bytes(
        filename: impl Into<String>,
        mime: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<Self> {
        Self::new(bytes, mime, filename).validated()
    }

    pub fn validated(self) -> Result<Self> {
        validate_user_artifact(&self)?;
        Ok(self)
    }
}

pub fn build_ai_behavior_response(output: AiBehaviorOutput) -> Result<Value> {
    build_ai_behavior_response_with_options(output, &TextResponseOptions::default())
}

pub fn build_ai_behavior_response_with_options(
    output: AiBehaviorOutput,
    options: &TextResponseOptions,
) -> Result<Value> {
    match output {
        AiBehaviorOutput::Text(content) => {
            build_text_response_with_options(content, vec![], options)
        }
        AiBehaviorOutput::Final(final_output) => {
            let attachments = materialize_user_artifacts(&final_output.artifacts, options)?;
            let text = final_output.text.unwrap_or_default();
            build_text_response_with_options(text, attachments, options)
        }
    }
}

pub fn materialize_user_artifacts(
    artifacts: &[AiUserArtifact],
    options: &TextResponseOptions,
) -> Result<Vec<BlobRef>> {
    let blob = BlobToolkit::new({
        let mut cfg = fluxbee_sdk::blob::BlobConfig::default();
        if let Some(root) = options.blob_root.as_ref() {
            cfg.blob_root = root.clone();
        }
        cfg
    })?;

    let mut refs = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        validate_user_artifact(artifact)?;
        let blob_ref = blob.put_bytes(&artifact.bytes, &artifact.filename, &artifact.mime)?;
        blob.promote(&blob_ref)?;
        refs.push(blob_ref);
    }
    Ok(refs)
}

pub fn allowed_user_artifact_mime(mime: &str) -> bool {
    normalized_user_artifact_extension_set(mime).is_some()
}

fn validate_user_artifact(artifact: &AiUserArtifact) -> Result<()> {
    if !allowed_user_artifact_mime(&artifact.mime) {
        return Err(AiSdkError::ArtifactMimeNotAllowed {
            mime: artifact.mime.clone(),
        });
    }
    validate_user_artifact_filename(&artifact.filename, &artifact.mime)
}

fn validate_user_artifact_filename(filename: &str, mime: &str) -> Result<()> {
    let trimmed = filename.trim();
    if trimmed.is_empty() {
        return Err(AiSdkError::ArtifactFilenameInvalid {
            detail: "filename is required".to_string(),
        });
    }
    if trimmed.len() > AI_USER_ARTIFACT_FILENAME_MAX_CHARS {
        return Err(AiSdkError::ArtifactFilenameInvalid {
            detail: format!("filename exceeds {AI_USER_ARTIFACT_FILENAME_MAX_CHARS} characters"),
        });
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(AiSdkError::ArtifactFilenameInvalid {
            detail: "path traversal is not allowed in filename".to_string(),
        });
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return Err(AiSdkError::ArtifactFilenameInvalid {
            detail: "filename contains control characters".to_string(),
        });
    }
    let extension = trimmed
        .rsplit_once('.')
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .ok_or_else(|| AiSdkError::ArtifactFilenameInvalid {
            detail: "filename must include an extension".to_string(),
        })?;
    let allowed_extensions = normalized_user_artifact_extension_set(mime).ok_or_else(|| {
        AiSdkError::ArtifactMimeNotAllowed {
            mime: mime.to_string(),
        }
    })?;
    if !allowed_extensions
        .iter()
        .any(|candidate| *candidate == extension)
    {
        return Err(AiSdkError::ArtifactFilenameInvalid {
            detail: format!("filename extension '.{extension}' does not match MIME '{mime}'"),
        });
    }
    Ok(())
}

fn normalized_user_artifact_extension_set(mime: &str) -> Option<&'static [&'static str]> {
    let normalized = mime.trim().to_ascii_lowercase();
    ALLOWED_USER_ARTIFACT_MIMES
        .iter()
        .find_map(|(candidate, extensions)| (*candidate == normalized).then_some(*extensions))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use fluxbee_sdk::payload::TextV1Payload;

    use super::*;

    fn temp_blob_root() -> PathBuf {
        let unique = format!(
            "fluxbee-ai-sdk-output-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be valid")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn text_output_keeps_current_text_only_contract() {
        let payload =
            build_ai_behavior_response(AiBehaviorOutput::text("respuesta")).expect("payload");
        let parsed = TextV1Payload::from_value(&payload).expect("text payload should parse");
        assert_eq!(parsed.content.as_deref(), Some("respuesta"));
        assert!(parsed.attachments.is_empty());
    }

    #[test]
    fn final_output_materializes_artifacts_to_attachments() {
        let root = temp_blob_root();
        let options = TextResponseOptions {
            blob_root: Some(root.clone()),
            ..TextResponseOptions::default()
        };
        let output = AiBehaviorOutput::final_output(AiFinalOutput::new(
            Some("aca esta".to_string()),
            vec![AiUserArtifact::new(
                b"col1,col2\n1,2\n".to_vec(),
                "text/csv",
                "reporte.csv",
            )],
        ));

        let payload = build_ai_behavior_response_with_options(output, &options).expect("payload");
        let parsed = TextV1Payload::from_value(&payload).expect("text payload should parse");
        assert_eq!(parsed.content.as_deref(), Some("aca esta"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].mime, "text/csv");
        assert_eq!(parsed.attachments[0].filename_original, "reporte.csv");

        let path = root
            .join("active")
            .join(
                &parsed.attachments[0]
                    .blob_name
                    .rsplit('_')
                    .next()
                    .expect("hash suffix")[..2],
            )
            .join(&parsed.attachments[0].blob_name);
        assert!(
            path.exists(),
            "artifact should be promoted to active blob storage"
        );
    }

    #[test]
    fn artifact_builder_rejects_disallowed_mime() {
        let err = AiUserArtifact::from_bytes("payload.bin", "application/octet-stream", b"x")
            .expect_err("disallowed MIME should fail");
        assert!(matches!(err, AiSdkError::ArtifactMimeNotAllowed { .. }));
    }

    #[test]
    fn artifact_builder_rejects_mismatched_extension() {
        let err = AiUserArtifact::from_text("reporte.csv", "hola")
            .expect_err("mismatched extension should fail");
        assert!(matches!(err, AiSdkError::ArtifactFilenameInvalid { .. }));
    }

    #[test]
    fn artifact_builders_create_supported_formats() {
        let txt = AiUserArtifact::from_text("nota.txt", "hola").expect("txt");
        assert_eq!(txt.mime, "text/plain");

        let json =
            AiUserArtifact::from_json("payload.json", &serde_json::json!({"a":1})).expect("json");
        assert_eq!(json.mime, "application/json");

        let md = AiUserArtifact::from_markdown("readme.md", "# Hola").expect("markdown");
        assert_eq!(md.mime, "text/markdown");

        let html = AiUserArtifact::from_html("index.html", "<p>Hola</p>").expect("html");
        assert_eq!(html.mime, "text/html");
    }
}
