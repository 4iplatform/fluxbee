use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::blob::{BlobError, BlobRef};

pub const TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    #[error("invalid payload contract: {0}")]
    InvalidContract(String),
    #[error("json payload error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("blob ref error: {0}")]
    Blob(#[from] BlobError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextV1Payload {
    #[serde(rename = "type")]
    pub payload_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_ref: Option<BlobRef>,
    #[serde(default)]
    pub attachments: Vec<BlobRef>,
}

impl TextV1Payload {
    pub fn new(content: impl Into<String>, attachments: Vec<BlobRef>) -> Self {
        Self {
            payload_type: "text".to_string(),
            content: Some(content.into()),
            content_ref: None,
            attachments,
        }
    }

    pub fn with_content_ref(content_ref: BlobRef, attachments: Vec<BlobRef>) -> Self {
        Self {
            payload_type: "text".to_string(),
            content: None,
            content_ref: Some(content_ref),
            attachments,
        }
    }

    pub fn validate(&self) -> Result<(), PayloadError> {
        if self.payload_type != "text" {
            return Err(PayloadError::InvalidContract(
                "field type must be 'text'".to_string(),
            ));
        }
        if self.content_ref.is_some() && self.content.is_some() {
            return Err(PayloadError::InvalidContract(
                "content and content_ref are mutually exclusive".to_string(),
            ));
        }
        if self.content_ref.is_none() && self.content.is_none() {
            return Err(PayloadError::InvalidContract(
                "either content or content_ref is required".to_string(),
            ));
        }
        if let Some(blob) = &self.content_ref {
            blob.validate()?;
        }
        for blob in &self.attachments {
            blob.validate()?;
        }
        Ok(())
    }

    pub fn to_value(&self) -> Result<Value, PayloadError> {
        self.validate()?;
        Ok(json!(self))
    }

    pub fn from_value(value: &Value) -> Result<Self, PayloadError> {
        let payload: TextV1Payload = serde_json::from_value(value.clone())?;
        payload.validate()?;
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_blob() -> BlobRef {
        BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "test_0123456789abcdef.png".to_string(),
            size: 10,
            mime: "image/png".to_string(),
            filename_original: "test.png".to_string(),
            spool_day: "2026-02-24".to_string(),
        }
    }

    #[test]
    fn text_payload_inline_roundtrip() {
        let payload = TextV1Payload::new("hola", vec![sample_blob()]);
        let value = payload.to_value().expect("serialize");
        let parsed = TextV1Payload::from_value(&value).expect("parse");
        assert_eq!(parsed.payload_type, "text");
        assert_eq!(parsed.content.as_deref(), Some("hola"));
        assert_eq!(parsed.content_ref, None);
        assert_eq!(parsed.attachments.len(), 1);
    }

    #[test]
    fn text_payload_content_ref_roundtrip() {
        let content_ref = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "content_0123456789abcdef.txt".to_string(),
            size: 100,
            mime: "text/plain".to_string(),
            filename_original: "content.txt".to_string(),
            spool_day: "2026-02-24".to_string(),
        };
        let payload = TextV1Payload::with_content_ref(content_ref, vec![]);
        let value = payload.to_value().expect("serialize");
        let parsed = TextV1Payload::from_value(&value).expect("parse");
        assert!(parsed.content.is_none());
        assert!(parsed.content_ref.is_some());
    }

    #[test]
    fn text_payload_rejects_both_content_and_content_ref() {
        let payload = TextV1Payload {
            payload_type: "text".to_string(),
            content: Some("hola".to_string()),
            content_ref: Some(sample_blob()),
            attachments: vec![],
        };
        assert!(payload.validate().is_err());
    }
}
