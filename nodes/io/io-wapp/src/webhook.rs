//! Pure parsing of the WhatsApp Cloud API webhook envelope (no I/O). The edge fans the RAW body to
//! every IO.wapp node; each node verifies the signature over the exact bytes, then extracts the
//! messages for ITS `phone_number_id` from this envelope. Shape (Meta):
//! `{object:"whatsapp_business_account", entry:[{id:<waba>, changes:[{field:"messages",
//!   value:{metadata:{phone_number_id,..}, contacts:[..], messages:[..], statuses:[..]}}]}]}`

use serde_json::Value;

/// One inbound WhatsApp message, flattened out of the webhook envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct WappInboundMessage {
    /// The receiving business number id (`value.metadata.phone_number_id`) — the self-select key.
    pub phone_number_id: String,
    /// The WABA id (`entry[].id`).
    pub waba_id: String,
    /// Sender (customer) WhatsApp id — E.164 digits (`messages[].from`).
    pub from_wa_id: String,
    /// Sender profile name when Meta includes it (`contacts[].profile.name`).
    pub profile_name: Option<String>,
    /// The WhatsApp message id (`messages[].id`, `wamid...`) — the dedup key.
    pub message_id: String,
    /// Unix seconds as sent by Meta (string in the payload).
    pub timestamp: Option<String>,
    pub kind: WappMessageKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WappMessageKind {
    /// `type:"text"` — the free-form body.
    Text { body: String },
    /// A media message (`image`/`document`/`audio`/`video`/`sticker`): the Graph `media id` to fetch
    /// plus an optional caption + mime. Download happens in the media phase.
    Media {
        media_type: String,
        media_id: String,
        caption: Option<String>,
        mime_type: Option<String>,
    },
    /// Anything else (location, contacts, reactions, interactive replies, …) — carried with its type
    /// so the caller can log/skip explicitly rather than silently.
    Other { message_type: String },
}

/// Extract every inbound message from a webhook envelope. Returns an empty vec for non-message
/// deliveries (status updates, unrelated `object`s) — those are legitimate webhooks to ack + ignore.
pub fn extract_inbound_messages(envelope: &Value) -> Vec<WappInboundMessage> {
    let mut out = Vec::new();
    if envelope.get("object").and_then(Value::as_str) != Some("whatsapp_business_account") {
        return out;
    }
    let Some(entries) = envelope.get("entry").and_then(Value::as_array) else {
        return out;
    };
    for entry in entries {
        let waba_id = entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(changes) = entry.get("changes").and_then(Value::as_array) else {
            continue;
        };
        for change in changes {
            if change.get("field").and_then(Value::as_str) != Some("messages") {
                continue;
            }
            let Some(value) = change.get("value") else {
                continue;
            };
            let phone_number_id = value
                .get("metadata")
                .and_then(|m| m.get("phone_number_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // contacts[] gives wa_id -> profile.name; usually one contact per delivery.
            let profile_name_for = |wa_id: &str| -> Option<String> {
                value
                    .get("contacts")
                    .and_then(Value::as_array)?
                    .iter()
                    .find(|c| c.get("wa_id").and_then(Value::as_str) == Some(wa_id))
                    .and_then(|c| c.get("profile"))
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            };
            let Some(messages) = value.get("messages").and_then(Value::as_array) else {
                continue; // statuses-only delivery (sent/delivered/read) — ack + ignore
            };
            for message in messages {
                // Trim the identifiers: `from`/`id` never carry meaningful whitespace, and the guard
                // MUST match the downstream trim-based validation. compute_thread_id (via the io_context)
                // rejects any value that TRIMS to empty and `.expect()`s the result — so a bare
                // `.is_empty()` guard would let a whitespace-only `from` (" ") through and panic the
                // inbound loop. Normalizing here also keeps the dedup key / external_id / thread_id stable.
                let from_wa_id = message
                    .get("from")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let message_id = message
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if from_wa_id.is_empty() || message_id.is_empty() {
                    continue; // not a relayable message without sender + dedup key
                }
                let timestamp = message
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let message_type = message
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let kind = match message_type {
                    "text" => WappMessageKind::Text {
                        body: message
                            .get("text")
                            .and_then(|t| t.get("body"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    },
                    media @ ("image" | "document" | "audio" | "video" | "sticker") => {
                        let media_obj = message.get(media);
                        WappMessageKind::Media {
                            media_type: media.to_string(),
                            media_id: media_obj
                                .and_then(|m| m.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            caption: media_obj
                                .and_then(|m| m.get("caption"))
                                .and_then(Value::as_str)
                                .map(ToString::to_string),
                            mime_type: media_obj
                                .and_then(|m| m.get("mime_type"))
                                .and_then(Value::as_str)
                                .map(ToString::to_string),
                        }
                    }
                    other => WappMessageKind::Other {
                        message_type: other.to_string(),
                    },
                };
                out.push(WappInboundMessage {
                    phone_number_id: phone_number_id.clone(),
                    waba_id: waba_id.clone(),
                    profile_name: profile_name_for(&from_wa_id),
                    from_wa_id,
                    message_id,
                    timestamp,
                    kind,
                });
            }
        }
    }
    out
}

/// Decode standard base64 (`A-Za-z0-9+/` with `=` padding) — the exact inverse of the edge's
/// `base64_encode` that produced `raw_body_base64`. `None` on any malformed input (default-deny: an
/// undecodable body can never verify).
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value_of(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let input = input.trim();
    if input.is_empty() {
        return Some(Vec::new());
    }
    if !input.len().is_multiple_of(4) {
        return None;
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    for (i, chunk) in bytes.chunks(4).enumerate() {
        let is_last = (i + 1) * 4 == bytes.len();
        let pad = chunk.iter().filter(|&&c| c == b'=').count();
        // Padding is only legal as the final 1-2 characters of the final chunk.
        if pad > 2 || (pad > 0 && (!is_last || chunk[..4 - pad].contains(&b'='))) {
            return None;
        }
        let v0 = value_of(chunk[0])?;
        let v1 = value_of(chunk[1])?;
        let v2 = if pad >= 2 { 0 } else { value_of(chunk[2])? };
        let v3 = if pad >= 1 { 0 } else { value_of(chunk[3])? };
        let n = (v0 << 18) | (v1 << 12) | (v2 << 6) | v3;
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Verify Meta's `X-Hub-Signature-256` over the RAW body bytes with the app-level `app_secret`
/// (`sha256=<hex hmac-sha256(app_secret, raw_body)>`). Constant-time via `Mac::verify_slice`
/// (mirrors src/mesh_hmac.rs). DEFAULT-DENY: a missing/malformed header, or any HMAC mismatch,
/// is `false` — unsigned is never fine.
pub fn verify_webhook_signature(app_secret: &str, raw_body: &[u8], signature_header: &str) -> bool {
    use hmac::{Hmac, Mac};
    let Some(expected) = parse_signature_header(signature_header) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(app_secret.as_bytes()) else {
        return false;
    };
    mac.update(raw_body);
    mac.verify_slice(&expected).is_ok()
}

/// Parse Meta's `X-Hub-Signature-256` header value (`sha256=<lowercase hex>`) into the raw HMAC
/// bytes. `None` for a missing prefix or non-hex payload — callers treat that as a failed signature
/// (default-deny), never as "unsigned is fine".
pub fn parse_signature_header(header: &str) -> Option<Vec<u8>> {
    let hex = header.trim().strip_prefix("sha256=")?;
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A realistic Meta webhook delivery: one text message to number 111 + a status-only change.
    fn sample_envelope() -> Value {
        json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "WABA-1",
                "changes": [
                    {
                        "field": "messages",
                        "value": {
                            "messaging_product": "whatsapp",
                            "metadata": { "display_phone_number": "15550001111", "phone_number_id": "111" },
                            "contacts": [{ "profile": { "name": "Ada" }, "wa_id": "5491100000000" }],
                            "messages": [{
                                "from": "5491100000000",
                                "id": "wamid.AAA==",
                                "timestamp": "1700000000",
                                "type": "text",
                                "text": { "body": "hola" }
                            }]
                        }
                    },
                    {
                        "field": "messages",
                        "value": {
                            "metadata": { "phone_number_id": "111" },
                            "statuses": [{ "id": "wamid.BBB==", "status": "delivered" }]
                        }
                    }
                ]
            }]
        })
    }

    #[test]
    fn extracts_text_message_with_binding_and_profile() {
        let msgs = extract_inbound_messages(&sample_envelope());
        assert_eq!(msgs.len(), 1, "statuses-only change must not produce a message");
        let m = &msgs[0];
        assert_eq!(m.phone_number_id, "111");
        assert_eq!(m.waba_id, "WABA-1");
        assert_eq!(m.from_wa_id, "5491100000000");
        assert_eq!(m.profile_name.as_deref(), Some("Ada"));
        assert_eq!(m.message_id, "wamid.AAA==");
        assert_eq!(m.kind, WappMessageKind::Text { body: "hola".into() });
    }

    #[test]
    fn extracts_media_and_other_kinds() {
        let envelope = json!({
            "object": "whatsapp_business_account",
            "entry": [{ "id": "W", "changes": [{ "field": "messages", "value": {
                "metadata": { "phone_number_id": "222" },
                "messages": [
                    { "from": "1", "id": "wamid.img", "type": "image",
                      "image": { "id": "MEDIA-9", "mime_type": "image/jpeg", "caption": "foto" } },
                    { "from": "1", "id": "wamid.loc", "type": "location", "location": {} }
                ]
            }}]}]
        });
        let msgs = extract_inbound_messages(&envelope);
        assert_eq!(msgs.len(), 2);
        assert_eq!(
            msgs[0].kind,
            WappMessageKind::Media {
                media_type: "image".into(),
                media_id: "MEDIA-9".into(),
                caption: Some("foto".into()),
                mime_type: Some("image/jpeg".into()),
            }
        );
        assert_eq!(msgs[1].kind, WappMessageKind::Other { message_type: "location".into() });
    }

    #[test]
    fn ignores_non_whatsapp_objects_and_incomplete_messages() {
        assert!(extract_inbound_messages(&json!({"object":"page","entry":[]})).is_empty());
        // a message with no id / no from is dropped (no dedup key / sender)
        let envelope = json!({
            "object": "whatsapp_business_account",
            "entry": [{ "id": "W", "changes": [{ "field": "messages", "value": {
                "metadata": { "phone_number_id": "1" },
                "messages": [{ "type": "text", "text": { "body": "x" } }]
            }}]}]
        });
        assert!(extract_inbound_messages(&envelope).is_empty());
    }

    #[test]
    fn drops_whitespace_only_from_or_id_so_thread_id_never_panics() {
        // A whitespace-only `from`/`id` is non-empty but TRIMS to empty; the downstream
        // compute_thread_id `.expect()`s a trim-valid conversation_id, so the guard MUST reject it here
        // (an `.is_empty()` guard would leak " " through and panic the inbound loop). Also asserts the
        // surviving identifiers are trimmed (stable dedup key / external_id / thread_id).
        let envelope = json!({
            "object": "whatsapp_business_account",
            "entry": [{ "id": "W", "changes": [{ "field": "messages", "value": {
                "metadata": { "phone_number_id": "1" },
                "messages": [
                    { "from": "   ", "id": "wamid.a", "type": "text", "text": { "body": "x" } },
                    { "from": "573001", "id": "  \t ", "type": "text", "text": { "body": "y" } },
                    { "from": " 573002 ", "id": " wamid.b ", "type": "text", "text": { "body": "z" } }
                ]
            }}]}]
        });
        let msgs = extract_inbound_messages(&envelope);
        assert_eq!(msgs.len(), 1, "only the third message has valid from + id");
        assert_eq!(msgs[0].from_wa_id, "573002");
        assert_eq!(msgs[0].message_id, "wamid.b");
    }

    #[test]
    fn base64_decode_roundtrips_and_rejects_garbage() {
        // Vectors matching the edge's encoder (standard alphabet + '=' padding).
        assert_eq!(base64_decode("aG9sYQ==").as_deref(), Some(b"hola".as_slice()));
        assert_eq!(base64_decode("aG9sYXM=").as_deref(), Some(b"holas".as_slice()));
        assert_eq!(base64_decode("aG9sYXMh").as_deref(), Some(b"holas!".as_slice()));
        assert_eq!(base64_decode("").as_deref(), Some(b"".as_slice()));
        assert_eq!(base64_decode("abc"), None); // bad length
        assert_eq!(base64_decode("a$c="), None); // bad alphabet
        assert_eq!(base64_decode("aG==aG=="), None); // padding not final
        // binary roundtrip
        let raw: Vec<u8> = (0u8..=255).collect();
        let enc = {
            // reuse the edge's algorithm inline to produce the fixture
            const ALPHABET: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for chunk in raw.chunks(3) {
                let n = ((chunk[0] as u32) << 16)
                    | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
                    | (*chunk.get(2).unwrap_or(&0) as u32);
                out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
                out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
                out.push(if chunk.len() > 1 { ALPHABET[((n >> 6) & 63) as usize] as char } else { '=' });
                out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
            }
            out
        };
        assert_eq!(base64_decode(&enc).as_deref(), Some(raw.as_slice()));
    }

    #[test]
    fn verify_webhook_signature_accepts_valid_and_denies_everything_else() {
        use hmac::{Hmac, Mac};
        let secret = "app-secret";
        let body = br#"{"object":"whatsapp_business_account"}"#;
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let header = format!("sha256={hex}");
        assert!(verify_webhook_signature(secret, body, &header));
        // wrong secret / tampered body / malformed header => deny
        assert!(!verify_webhook_signature("other-secret", body, &header));
        assert!(!verify_webhook_signature(secret, b"tampered", &header));
        assert!(!verify_webhook_signature(secret, body, "sha1=abcd"));
        assert!(!verify_webhook_signature(secret, body, ""));
    }

    #[test]
    fn parses_signature_header_and_rejects_garbage() {
        assert_eq!(
            parse_signature_header("sha256=0aff"),
            Some(vec![0x0a, 0xff])
        );
        assert_eq!(parse_signature_header("sha256="), None);
        assert_eq!(parse_signature_header("sha1=abcd"), None);
        assert_eq!(parse_signature_header("sha256=xyz1"), None);
        assert_eq!(parse_signature_header("sha256=abc"), None); // odd length
    }
}
