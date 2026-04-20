use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use base64::Engine as _;
use chrono::SecondsFormat;
use chrono::Utc;
use fluxbee_sdk::blob::BlobToolkit;
use fluxbee_sdk::protocol::Message as WireMessage;
use hmac::{Hmac, Mac};
use io_common::io_context::{extract_webhook_post_target, WebhookPostTarget};
use io_common::text_v1_blob::{resolve_text_v1_for_outbound, IoTextBlobConfig};
use serde::Serialize;
use sha2::Sha256;

use crate::auth::resolve_secret_reference;
use crate::{ApiAuthRegistry, HttpState, IntegrationRuntime, WebhookRuntime};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
struct PreparedWebhookDelivery {
    integration_id: String,
    request_id: String,
    trace_id: String,
    delivery_id: String,
    url: String,
    timeout_ms: u64,
    max_retries: u64,
    initial_backoff_ms: u64,
    max_backoff_ms: u64,
    raw_body: Vec<u8>,
    signature_secret: String,
}

#[derive(Debug, Serialize)]
struct WebhookEnvelope<'a> {
    schema_version: u32,
    delivery_id: &'a str,
    event_type: &'a str,
    occurred_at: String,
    integration_id: &'a str,
    tenant_id: &'a str,
    request: WebhookRequest<'a>,
    result: WebhookResult,
    source: WebhookSource<'a>,
}

#[derive(Debug, Serialize)]
struct WebhookRequest<'a> {
    request_id: &'a str,
    trace_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ilk: Option<&'a str>,
}

#[derive(Debug)]
struct WebhookResult {
    final_field: bool,
    status: &'static str,
    message: Option<WebhookMessage>,
    error: Option<WebhookError>,
}

#[derive(Debug, Serialize)]
struct WebhookMessage {
    text: String,
    attachments: Vec<WebhookAttachment>,
}

#[derive(Debug, Serialize)]
struct WebhookAttachment {
    name: String,
    mime_type: String,
    size_bytes: u64,
    content_base64: String,
}

#[derive(Debug, Serialize)]
struct WebhookError {
    code: &'static str,
    message: String,
    retryable: bool,
}

#[derive(Debug, Serialize)]
struct WebhookSource<'a> {
    node_name: &'a str,
}

impl Serialize for WebhookResult {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("final", &self.final_field)?;
        map.serialize_entry("status", self.status)?;
        if let Some(message) = &self.message {
            map.serialize_entry("message", message)?;
        }
        if let Some(error) = &self.error {
            map.serialize_entry("error", error)?;
        } else {
            map.serialize_entry("error", &Option::<WebhookError>::None)?;
        }
        map.end()
    }
}

pub(crate) async fn maybe_deliver_webhook_outbound(
    state: &Arc<HttpState>,
    msg: &WireMessage,
) -> bool {
    let Some(target) = extract_webhook_target_from_message(msg) else {
        return false;
    };

    let integration = {
        let registry = state.auth_registry.read().await;
        resolve_webhook_integration(&registry, &target)
    };
    let Some(integration) = integration else {
        tracing::warn!(
            integration_id = %target.integration_id,
            request_id = %target.request_id,
            trace_id = %target.trace_id,
            "io-api webhook outbound dropped: integration not found"
        );
        return true;
    };
    let Some(webhook) = integration.webhook.clone() else {
        tracing::debug!(
            integration_id = %target.integration_id,
            request_id = %target.request_id,
            trace_id = %target.trace_id,
            "io-api webhook outbound skipped: integration has no enabled webhook"
        );
        return true;
    };

    let prepared = match prepare_webhook_delivery(state.as_ref(), msg, &integration, &webhook).await {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                integration_id = %target.integration_id,
                request_id = %target.request_id,
                trace_id = %target.trace_id,
                error = %err,
                "io-api webhook outbound preparation failed"
            );
            return true;
        }
    };
    tracing::debug!(
        integration_id = %prepared.integration_id,
        request_id = %prepared.request_id,
        trace_id = %prepared.trace_id,
        delivery_id = %prepared.delivery_id,
        "io-api webhook outbound dispatching final callback"
    );
    let http = state.webhook_http.clone();
    tokio::spawn(async move {
        if let Err(err) = deliver_webhook_outbound_with_retries(http, prepared).await {
            tracing::warn!(error = %err, "io-api webhook outbound delivery failed");
        }
    });
    true
}

fn extract_webhook_target_from_message(msg: &WireMessage) -> Option<WebhookPostTarget> {
    let meta_context = msg.meta.context.as_ref()?;
    extract_webhook_post_target(meta_context)
}

fn resolve_webhook_integration(
    registry: &ApiAuthRegistry,
    target: &WebhookPostTarget,
) -> Option<IntegrationRuntime> {
    registry.integrations.get(&target.integration_id).cloned()
}

async fn prepare_webhook_delivery(
    state: &HttpState,
    msg: &WireMessage,
    integration: &IntegrationRuntime,
    webhook: &WebhookRuntime,
) -> Result<PreparedWebhookDelivery> {
    let target = extract_webhook_post_target(
        msg.meta
            .context
            .as_ref()
            .expect("checked before calling preparation"),
    )
    .expect("checked before calling preparation");
    let delivery_id = format!("dly_{}", msg.routing.trace_id);
    let body = build_webhook_body(
        &state.node_name,
        state.blob_toolkit.as_ref(),
        &state.blob_payload_cfg,
        msg,
        &target,
        integration,
        &delivery_id,
    )
    .await?;
    let raw_body = serde_json::to_vec(&body)?;
    let signature_secret = resolve_secret_reference(&state.node_name, &webhook.secret_ref, None)?;
    Ok(PreparedWebhookDelivery {
        integration_id: integration.integration_id.clone(),
        request_id: target.request_id,
        trace_id: target.trace_id,
        delivery_id,
        url: webhook.url.clone(),
        timeout_ms: webhook.timeout_ms,
        max_retries: webhook.max_retries,
        initial_backoff_ms: webhook.initial_backoff_ms,
        max_backoff_ms: webhook.max_backoff_ms,
        raw_body,
        signature_secret,
    })
}

async fn deliver_webhook_outbound_with_retries(
    http: reqwest::Client,
    delivery: PreparedWebhookDelivery,
) -> Result<()> {
    let total_attempts = delivery.max_retries.saturating_add(1);
    let mut attempt: u64 = 1;
    loop {
        match deliver_webhook_attempt(&http, &delivery).await {
            Ok(()) => {
                tracing::info!(
                    integration_id = %delivery.integration_id,
                    request_id = %delivery.request_id,
                    trace_id = %delivery.trace_id,
                    delivery_id = %delivery.delivery_id,
                    attempt,
                    "io-api webhook outbound delivered"
                );
                return Ok(());
            }
            Err(err) if attempt < total_attempts => {
                let backoff_ms = compute_retry_backoff_ms(
                    delivery.initial_backoff_ms,
                    delivery.max_backoff_ms,
                    attempt,
                    &delivery.delivery_id,
                );
                tracing::debug!(
                    integration_id = %delivery.integration_id,
                    request_id = %delivery.request_id,
                    trace_id = %delivery.trace_id,
                    delivery_id = %delivery.delivery_id,
                    attempt,
                    next_backoff_ms = backoff_ms,
                    error = %err,
                    "io-api webhook outbound retry scheduled"
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                attempt = attempt.saturating_add(1);
            }
            Err(err) => {
                tracing::warn!(
                    integration_id = %delivery.integration_id,
                    request_id = %delivery.request_id,
                    trace_id = %delivery.trace_id,
                    delivery_id = %delivery.delivery_id,
                    attempt,
                    error = %err,
                    "io-api webhook outbound retries exhausted"
                );
                anyhow::bail!(err);
            }
        }
    }
}

async fn deliver_webhook_attempt(http: &reqwest::Client, delivery: &PreparedWebhookDelivery) -> Result<()> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let signature = compute_signature(
        delivery.signature_secret.as_bytes(),
        timestamp,
        &delivery.raw_body,
    )?;
    let response = http
        .post(&delivery.url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("X-Fluxbee-Delivery-Id", &delivery.delivery_id)
        .header("X-Fluxbee-Timestamp", timestamp.to_string())
        .header("X-Fluxbee-Signature", format!("v1={signature}"))
        .timeout(std::time::Duration::from_millis(delivery.timeout_ms))
        .body(delivery.raw_body.clone())
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("webhook endpoint returned non-2xx status {}", response.status());
    }
    Ok(())
}

fn compute_retry_backoff_ms(
    initial_backoff_ms: u64,
    max_backoff_ms: u64,
    attempt: u64,
    delivery_id: &str,
) -> u64 {
    let exponent = attempt.saturating_sub(1).min(16);
    let base = initial_backoff_ms
        .saturating_mul(1u64 << exponent)
        .min(max_backoff_ms);
    let jitter_span = ((base as f64) * 0.2_f64) as u64;
    let jitter = deterministic_jitter(delivery_id, attempt, jitter_span);
    base.saturating_add(jitter).min(max_backoff_ms)
}

fn deterministic_jitter(seed: &str, attempt: u64, max_jitter_ms: u64) -> u64 {
    if max_jitter_ms == 0 {
        return 0;
    }
    let mut acc: u64 = 1469598103934665603;
    for byte in seed.as_bytes() {
        acc ^= *byte as u64;
        acc = acc.wrapping_mul(1099511628211);
    }
    acc ^= attempt;
    acc = acc.wrapping_mul(1099511628211);
    acc % max_jitter_ms.saturating_add(1)
}

async fn build_webhook_body<'a>(
    node_name: &'a str,
    blob_toolkit: &'a BlobToolkit,
    blob_payload_cfg: &'a IoTextBlobConfig,
    msg: &'a WireMessage,
    target: &'a WebhookPostTarget,
    integration: &'a IntegrationRuntime,
    delivery_id: &'a str,
) -> Result<WebhookEnvelope<'a>> {
    let result = match resolve_text_v1_for_outbound(
        blob_toolkit,
        blob_payload_cfg,
        &msg.payload,
        false,
    )
    .await
    {
        Ok(resolved) => {
            let mut attachments = Vec::with_capacity(resolved.attachments.len());
            for attachment in resolved.attachments {
                let bytes = std::fs::read(&attachment.path)?;
                attachments.push(WebhookAttachment {
                    name: attachment.blob_ref.filename_original,
                    mime_type: attachment.blob_ref.mime,
                    size_bytes: attachment.blob_ref.size,
                    content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                });
            }
            WebhookResult {
                final_field: true,
                status: "ok",
                message: Some(WebhookMessage {
                    text: resolved.text,
                    attachments,
                }),
                error: None,
            }
        }
        Err(err) => {
            let code = match err.canonical_code() {
                "unsupported_attachment_mime" | "too_many_attachments" | "BLOB_TOO_LARGE" => {
                    "unsupported_outbound_attachment"
                }
                _ => "unsupported_outbound_payload",
            };
            WebhookResult {
                final_field: true,
                status: "error",
                message: None,
                error: Some(WebhookError {
                    code,
                    message: err.to_string(),
                    retryable: false,
                }),
            }
        }
    };

    Ok(WebhookEnvelope {
        schema_version: 1,
        delivery_id,
        event_type: "io.api.final",
        occurred_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        integration_id: &integration.integration_id,
        tenant_id: &integration.tenant_id,
        request: WebhookRequest {
            request_id: &target.request_id,
            trace_id: &target.trace_id,
            ilk: msg.meta.src_ilk.as_deref().filter(|value| !value.is_empty()),
        },
        result,
        source: WebhookSource {
            node_name,
        },
    })
}

fn compute_signature(secret: &[u8], timestamp: u64, raw_body: &[u8]) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(raw_body);
    let bytes = mac.finalize().into_bytes();
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use fluxbee_sdk::blob::{BlobConfig, BlobToolkit};
    use fluxbee_sdk::payload::TextV1Payload;
    use fluxbee_sdk::protocol::{Destination, Meta, Routing};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Debug, Clone)]
    struct CapturedRequest {
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    #[derive(Clone)]
    struct FakeWebhookState {
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
        statuses: Arc<Mutex<Vec<StatusCode>>>,
    }

    fn temp_root(label: &str) -> PathBuf {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or(0);
        let root = std::env::temp_dir()
            .join(format!("io-api-webhook-{label}-{}-{now_ms}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn test_toolkit(label: &str) -> (BlobToolkit, PathBuf) {
        let root = temp_root(label);
        let toolkit = BlobToolkit::new(BlobConfig {
            blob_root: root.clone(),
            name_max_chars: fluxbee_sdk::blob::BLOB_NAME_MAX_CHARS,
            max_blob_bytes: None,
        })
        .expect("blob toolkit");
        (toolkit, root)
    }

    fn sample_target() -> WebhookPostTarget {
        WebhookPostTarget {
            integration_id: "int_partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            request_id: "req_123".to_string(),
            trace_id: "trace_123".to_string(),
            reply_mode: "final_only".to_string(),
        }
    }

    fn sample_integration() -> IntegrationRuntime {
        IntegrationRuntime {
            integration_id: "int_partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            final_reply_required: true,
            webhook: Some(WebhookRuntime {
                url: "https://example.com/webhook".to_string(),
                secret_ref: "env:IGNORED".to_string(),
                timeout_ms: 5000,
                max_retries: 1,
                initial_backoff_ms: 5,
                max_backoff_ms: 10,
            }),
        }
    }

    fn sample_message(payload: Value) -> WireMessage {
        WireMessage {
            routing: Routing {
                src: "AI.chat@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("IO.api.support@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace_delivery_123".to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                msg: Some("reply".to_string()),
                src_ilk: Some("ilk:123".to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    async fn fake_webhook_handler(
        State(state): State<FakeWebhookState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        let mut captured = HashMap::new();
        for (name, value) in &headers {
            if let Ok(value) = value.to_str() {
                captured.insert(name.as_str().to_string(), value.to_string());
            }
        }
        state.requests.lock().await.push(CapturedRequest {
            headers: captured,
            body: body.to_vec(),
        });
        let mut statuses = state.statuses.lock().await;
        if statuses.is_empty() {
            StatusCode::NO_CONTENT
        } else {
            statuses.remove(0)
        }
    }

    #[test]
    fn compute_signature_is_stable() {
        let raw_body = br#"{"ok":true}"#;
        let sig = compute_signature(b"secret", 1776686400, raw_body).expect("signature");
        assert_eq!(
            sig,
            "d250b2b4c679df78331349d88323b86d9e9c52042d4f42f1e7794d34512a9e0b"
        );
    }

    #[test]
    fn retry_backoff_is_capped_and_monotonic() {
        let a1 = compute_retry_backoff_ms(1000, 30_000, 1, "dly_1");
        let a2 = compute_retry_backoff_ms(1000, 30_000, 2, "dly_1");
        let a3 = compute_retry_backoff_ms(1000, 30_000, 3, "dly_1");
        assert!(a1 >= 1000);
        assert!(a2 >= 2000);
        assert!(a3 >= 4000);
        assert!(a1 <= a2);
        assert!(a2 <= a3);
        assert!(a3 <= 30_000);
    }

    #[test]
    fn retry_backoff_uses_stable_jitter_per_delivery_and_attempt() {
        let left = compute_retry_backoff_ms(1000, 30_000, 2, "dly_abc");
        let right = compute_retry_backoff_ms(1000, 30_000, 2, "dly_abc");
        let other = compute_retry_backoff_ms(1000, 30_000, 2, "dly_xyz");
        assert_eq!(left, right);
        assert_ne!(left, other);
    }

    #[tokio::test]
    async fn build_webhook_body_maps_text_payload_to_ok_message() {
        let (toolkit, root) = test_toolkit("body-text");
        let cfg = IoTextBlobConfig::default();
        let payload = TextV1Payload::new("hola webhook", vec![])
            .to_value()
            .expect("payload value");
        let message = sample_message(payload);
        let target = sample_target();
        let integration = sample_integration();

        let body = build_webhook_body(
            "IO.api.support@motherbee",
            &toolkit,
            &cfg,
            &message,
            &target,
            &integration,
            "dly_trace_delivery_123",
        )
        .await
        .expect("webhook body");

        assert_eq!(body.integration_id, "int_partner1");
        assert_eq!(body.tenant_id, "tnt:partner1");
        assert_eq!(body.request.request_id, "req_123");
        assert_eq!(body.request.trace_id, "trace_123");
        assert_eq!(body.request.ilk, Some("ilk:123"));
        assert_eq!(body.result.status, "ok");
        assert!(body.result.final_field);
        assert_eq!(
            body.result.message.as_ref().map(|value| value.text.as_str()),
            Some("hola webhook")
        );
        assert!(body
            .result
            .message
            .as_ref()
            .is_some_and(|value| value.attachments.is_empty()));
        assert!(body.result.error.is_none());
        assert_eq!(body.source.node_name, "IO.api.support@motherbee");

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn build_webhook_body_materializes_inline_attachment_base64() {
        let (toolkit, root) = test_toolkit("body-attachment");
        let cfg = IoTextBlobConfig::default();
        let attachment_bytes = b"attachment-data";
        let attachment = toolkit
            .put_bytes(attachment_bytes, "factura.pdf", "application/pdf")
            .expect("put attachment");
        toolkit.promote(&attachment).expect("promote attachment");
        let payload = TextV1Payload::new("hola con adjunto", vec![attachment.clone()])
            .to_value()
            .expect("payload value");
        let message = sample_message(payload);
        let target = sample_target();
        let integration = sample_integration();

        let body = build_webhook_body(
            "IO.api.support@motherbee",
            &toolkit,
            &cfg,
            &message,
            &target,
            &integration,
            "dly_trace_delivery_123",
        )
        .await
        .expect("webhook body");

        let attachments = &body
            .result
            .message
            .as_ref()
            .expect("message")
            .attachments;
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].name, "factura.pdf");
        assert_eq!(attachments[0].mime_type, "application/pdf");
        assert_eq!(attachments[0].size_bytes, attachment.size);
        assert_eq!(
            attachments[0].content_base64,
            base64::engine::general_purpose::STANDARD.encode(attachment_bytes)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn build_webhook_body_degrades_unsupported_payload_to_terminal_error() {
        let (toolkit, root) = test_toolkit("body-error");
        let cfg = IoTextBlobConfig::default();
        let message = sample_message(serde_json::json!({
            "type": "custom_payload",
            "content": "unsupported"
        }));
        let target = sample_target();
        let integration = sample_integration();

        let body = build_webhook_body(
            "IO.api.support@motherbee",
            &toolkit,
            &cfg,
            &message,
            &target,
            &integration,
            "dly_trace_delivery_123",
        )
        .await
        .expect("webhook body");

        assert_eq!(body.result.status, "error");
        assert!(body.result.message.is_none());
        assert_eq!(
            body.result.error.as_ref().map(|value| value.code),
            Some("unsupported_outbound_payload")
        );
        assert_eq!(
            body.result.error.as_ref().map(|value| value.retryable),
            Some(false)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn extract_webhook_target_from_message_returns_none_for_non_terminal_message() {
        let message = sample_message(serde_json::json!({
            "type": "text",
            "content": "hola"
        }));
        assert!(extract_webhook_target_from_message(&message).is_none());
    }

    #[test]
    fn resolve_webhook_integration_returns_none_for_invalid_integration() {
        let registry = ApiAuthRegistry {
            keys: Vec::new(),
            integrations: HashMap::from([(
                "int_partner1".to_string(),
                sample_integration(),
            )]),
        };
        let mut target = sample_target();
        target.integration_id = "int_missing".to_string();
        assert!(resolve_webhook_integration(&registry, &target).is_none());
    }

    #[tokio::test]
    async fn webhook_delivery_retries_and_preserves_signature_headers() {
        let requests = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
        let statuses = Arc::new(Mutex::new(vec![
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::NO_CONTENT,
        ]));
        let app = Router::new()
            .route("/", post(fake_webhook_handler))
            .with_state(FakeWebhookState {
                requests: requests.clone(),
                statuses,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fake webhook");
        });

        let raw_body = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "delivery_id": "dly_trace_delivery_123",
            "result": {"final": true, "status": "ok"}
        }))
        .expect("raw body");
        let delivery = PreparedWebhookDelivery {
            integration_id: "int_partner1".to_string(),
            request_id: "req_123".to_string(),
            trace_id: "trace_123".to_string(),
            delivery_id: "dly_trace_delivery_123".to_string(),
            url: format!("http://{addr}/"),
            timeout_ms: 5_000,
            max_retries: 1,
            initial_backoff_ms: 1,
            max_backoff_ms: 5,
            raw_body: raw_body.clone(),
            signature_secret: "topsecret".to_string(),
        };

        deliver_webhook_outbound_with_retries(reqwest::Client::new(), delivery)
            .await
            .expect("delivery should succeed after retry");

        let captured = requests.lock().await.clone();
        assert_eq!(captured.len(), 2);
        for request in &captured {
            assert_eq!(
                request.headers.get("content-type").map(String::as_str),
                Some("application/json")
            );
            assert_eq!(
                request
                    .headers
                    .get("x-fluxbee-delivery-id")
                    .map(String::as_str),
                Some("dly_trace_delivery_123")
            );
            assert_eq!(request.body, raw_body);
            let timestamp = request
                .headers
                .get("x-fluxbee-timestamp")
                .expect("timestamp header")
                .parse::<u64>()
                .expect("timestamp parse");
            let signature = request
                .headers
                .get("x-fluxbee-signature")
                .and_then(|value| value.strip_prefix("v1="))
                .expect("signature header");
            let expected = compute_signature(b"topsecret", timestamp, &request.body)
                .expect("expected signature");
            assert_eq!(signature, expected);
        }

        server.abort();
    }
}
