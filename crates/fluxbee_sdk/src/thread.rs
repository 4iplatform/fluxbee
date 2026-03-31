use sha2::{Digest, Sha256};

const THREAD_ID_PREFIX: &str = "thread:sha256:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadIdInput<'a> {
    NativeThread {
        channel_type: &'a str,
        entrypoint_id: Option<&'a str>,
        conversation_id: &'a str,
        native_thread_id: &'a str,
    },
    PersistentChannel {
        channel_type: &'a str,
        entrypoint_id: Option<&'a str>,
        conversation_id: &'a str,
    },
    DirectPair {
        channel_type: &'a str,
        entrypoint_id: Option<&'a str>,
        party_a: &'a str,
        party_b: &'a str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThreadIdError {
    #[error("thread id input field '{field}' is missing or empty")]
    MissingField { field: &'static str },
}

pub fn compute_thread_id(input: ThreadIdInput<'_>) -> Result<String, ThreadIdError> {
    let material = canonical_thread_material(input)?;
    let digest = Sha256::digest(material.as_bytes());
    Ok(format!("{THREAD_ID_PREFIX}{digest:x}"))
}

fn canonical_thread_material(input: ThreadIdInput<'_>) -> Result<String, ThreadIdError> {
    match input {
        ThreadIdInput::NativeThread {
            channel_type,
            entrypoint_id,
            conversation_id,
            native_thread_id,
        } => Ok(format!(
            "v2|kind=native_thread|channel_type={}|entrypoint_id={}|conversation_id={}|native_thread_id={}",
            normalize_channel_type(channel_type)?,
            normalize_optional(entrypoint_id),
            normalize_required("conversation_id", conversation_id)?,
            normalize_required("native_thread_id", native_thread_id)?,
        )),
        ThreadIdInput::PersistentChannel {
            channel_type,
            entrypoint_id,
            conversation_id,
        } => Ok(format!(
            "v2|kind=persistent_channel|channel_type={}|entrypoint_id={}|conversation_id={}",
            normalize_channel_type(channel_type)?,
            normalize_optional(entrypoint_id),
            normalize_required("conversation_id", conversation_id)?,
        )),
        ThreadIdInput::DirectPair {
            channel_type,
            entrypoint_id,
            party_a,
            party_b,
        } => {
            let party_a = normalize_required("party_a", party_a)?;
            let party_b = normalize_required("party_b", party_b)?;
            let (party_low, party_high) = if party_a <= party_b {
                (party_a, party_b)
            } else {
                (party_b, party_a)
            };
            Ok(format!(
                "v2|kind=direct_pair|channel_type={}|entrypoint_id={}|party_a={}|party_b={}",
                normalize_channel_type(channel_type)?,
                normalize_optional(entrypoint_id),
                party_low,
                party_high,
            ))
        }
    }
}

fn normalize_channel_type(value: &str) -> Result<String, ThreadIdError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(ThreadIdError::MissingField {
            field: "channel_type",
        });
    }
    Ok(normalized)
}

fn normalize_required(field: &'static str, value: &str) -> Result<String, ThreadIdError> {
    let normalized = value.trim().to_string();
    if normalized.is_empty() {
        return Err(ThreadIdError::MissingField { field });
    }
    Ok(normalized)
}

fn normalize_optional(value: Option<&str>) -> String {
    value
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::{compute_thread_id, ThreadIdError, ThreadIdInput};

    #[test]
    fn native_thread_is_stable_and_namespaced() {
        let left = compute_thread_id(ThreadIdInput::NativeThread {
            channel_type: "slack",
            entrypoint_id: Some("T123"),
            conversation_id: "C789",
            native_thread_id: "171234.567",
        })
        .expect("thread id");
        let right = compute_thread_id(ThreadIdInput::NativeThread {
            channel_type: " SLACK ",
            entrypoint_id: Some(" T123 "),
            conversation_id: " C789 ",
            native_thread_id: " 171234.567 ",
        })
        .expect("thread id");
        assert_eq!(left, right);
        assert!(left.starts_with("thread:sha256:"));
    }

    #[test]
    fn direct_pair_is_order_insensitive() {
        let left = compute_thread_id(ThreadIdInput::DirectPair {
            channel_type: "email",
            entrypoint_id: Some("tenant-a"),
            party_a: "alice@example.com",
            party_b: "bob@example.com",
        })
        .expect("thread id");
        let right = compute_thread_id(ThreadIdInput::DirectPair {
            channel_type: "email",
            entrypoint_id: Some("tenant-a"),
            party_a: "bob@example.com",
            party_b: "alice@example.com",
        })
        .expect("thread id");
        assert_eq!(left, right);
    }

    #[test]
    fn persistent_channel_differs_from_native_thread() {
        let channel = compute_thread_id(ThreadIdInput::PersistentChannel {
            channel_type: "slack",
            entrypoint_id: Some("T123"),
            conversation_id: "C789",
        })
        .expect("thread id");
        let native = compute_thread_id(ThreadIdInput::NativeThread {
            channel_type: "slack",
            entrypoint_id: Some("T123"),
            conversation_id: "C789",
            native_thread_id: "171234.567",
        })
        .expect("thread id");
        assert_ne!(channel, native);
    }

    #[test]
    fn missing_required_field_is_rejected() {
        let err = compute_thread_id(ThreadIdInput::PersistentChannel {
            channel_type: "slack",
            entrypoint_id: Some("T123"),
            conversation_id: "   ",
        })
        .expect_err("missing conversation_id");
        assert_eq!(
            err,
            ThreadIdError::MissingField {
                field: "conversation_id"
            }
        );
    }
}
