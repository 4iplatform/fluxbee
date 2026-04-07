use fluxbee_sdk::blob::{BlobRef, BlobToolkit};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::Result;
use crate::text_payload::{build_text_response_with_options, TextResponseOptions};

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
        let blob_ref = blob.put_bytes(&artifact.bytes, &artifact.filename, &artifact.mime)?;
        blob.promote(&blob_ref)?;
        refs.push(blob_ref);
    }
    Ok(refs)
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
}
