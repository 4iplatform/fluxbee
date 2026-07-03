use std::fmt::{Display, Formatter};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

/// Header the IO.linkedhelper node uses to identify the calling adapter.
const ADAPTER_ID_HEADER: &str = "X-Fluxbee-Adapter-Id";

/**
 * Machine-actionable control block the IO.linkedhelper node attaches to every
 * `/v1/poll` response (success and reject). It lets the node — not a permanent
 * Cloud heartbeat — tell the adapter what to do next: keep operating, pause
 * (operationally disabled), or reopen the administrative cycle against Fluxbee
 * Cloud (reenroll / reprovision). Unknown fields/values are tolerated.
 */
#[derive(Debug, Clone, Deserialize)]
pub struct NodeControl {
    #[serde(default)]
    pub operational_state: Option<String>,
    #[serde(default)]
    pub directive: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub retry_after_seconds: Option<u64>,
}

/// Directive values understood by the adapter (mirrors the node's `poll_directive`).
pub mod directive {
    pub const CONTINUE: &str = "continue";
    pub const PAUSE: &str = "pause";
    pub const REENROLL: &str = "reenroll";
    pub const REPROVISION: &str = "reprovision";
    pub const RETRY: &str = "retry";
}

/**
 * Minimal `/v1/poll` request body understood by the IO.linkedhelper node. In
 * this phase the adapter only sends heartbeats (no events yet), matching the
 * node's `mode = "heartbeat"` branch.
 */
#[derive(Debug, Serialize)]
struct NodePollRequest<'a> {
    request_id: &'a str,
    adapter_id: &'a str,
    managed_instance_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_instance_id: Option<&'a str>,
    mode: &'a str,
}

/**
 * Successful outcome of a node heartbeat.
 */
#[derive(Debug)]
pub struct NodeHeartbeatOutcome {
    pub status_code: u16,
    pub ok: bool,
    /// Control block returned by the node (directive + operational state).
    pub control: Option<NodeControl>,
}

/**
 * Structured error returned when reporting to the node fails (transport or
 * non-2xx). Never aborts the adapter cycle; it is surfaced per binding.
 */
#[derive(Debug)]
pub struct NodeReportError {
    pub status_code: Option<u16>,
    pub message: String,
    /// Stable node error code parsed from the reject body, when present.
    pub error_code: Option<String>,
    /// Control block parsed from the reject body (directive to act on), when present.
    pub control: Option<NodeControl>,
}

impl Display for NodeReportError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.status_code {
            Some(status_code) => {
                write!(f, "node report failed with status {}: {}", status_code, self.message)
            }
            None => write!(f, "node report failed: {}", self.message),
        }
    }
}

impl std::error::Error for NodeReportError {}

/// Parses the optional `control` block out of a node JSON response body.
fn parse_node_control(body: &serde_json::Value) -> Option<NodeControl> {
    body.get("control")
        .cloned()
        .and_then(|value| serde_json::from_value::<NodeControl>(value).ok())
}

/**
 * Sends one heartbeat poll to the IO.linkedhelper node at `report_url` using the
 * adapter credentials. Proves the intermediate `direct_node_http` path
 * end-to-end (auth + reachability) without yet delivering real events.
 */
pub fn send_node_heartbeat(
    report_url: &str,
    adapter_id: &str,
    adapter_secret: &str,
    managed_instance_id: &str,
    local_instance_id: Option<&str>,
    request_id: &str,
) -> Result<NodeHeartbeatOutcome, NodeReportError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| NodeReportError {
            status_code: None,
            message: error.to_string(),
            error_code: None,
            control: None,
        })?;

    let body = NodePollRequest {
        request_id,
        adapter_id,
        managed_instance_id,
        local_instance_id,
        mode: "heartbeat",
    };

    let response = client
        .post(report_url)
        .header(CONTENT_TYPE, "application/json")
        .header(ADAPTER_ID_HEADER, adapter_id)
        .header(AUTHORIZATION, format!("Bearer {}", adapter_secret))
        .json(&body)
        .send()
        .map_err(|error| NodeReportError {
            status_code: error.status().map(|value| value.as_u16()),
            message: error.to_string(),
            error_code: None,
            control: None,
        })?;

    let status_code = response.status().as_u16();
    if !response.status().is_success() {
        // Parse the reject body for the stable error_code + control directive so
        // the caller can decide whether to reconsider (reprovision/reenroll/pause)
        // rather than treating it as an opaque failure.
        let text = response
            .text()
            .unwrap_or_else(|_| String::from("unknown node error"));
        let (error_code, control) = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .map(|body| {
                (
                    body.get("error_code")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    parse_node_control(&body),
                )
            })
            .unwrap_or((None, None));
        return Err(NodeReportError {
            status_code: Some(status_code),
            message: text,
            error_code,
            control,
        });
    }

    let parsed: serde_json::Value = response.json().map_err(|error| NodeReportError {
        status_code: Some(status_code),
        message: format!("failed to parse node response: {}", error),
        error_code: None,
        control: None,
    })?;
    let ok = parsed.get("ok").and_then(|value| value.as_bool()).unwrap_or(false);
    let control = parse_node_control(&parsed);

    Ok(NodeHeartbeatOutcome { status_code, ok, control })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn parse_node_control_extracts_all_fields() {
        let body = serde_json::json!({
            "ok": true,
            "control": {
                "operational_state": "enabled",
                "directive": "continue",
                "reason": null,
                "retry_after_seconds": null
            }
        });
        let control = parse_node_control(&body).expect("control present");
        assert_eq!(control.operational_state.as_deref(), Some("enabled"));
        assert_eq!(control.directive.as_deref(), Some("continue"));
        assert!(control.reason.is_none());
    }

    #[test]
    fn parse_node_control_absent_is_none() {
        let body = serde_json::json!({ "ok": true });
        assert!(parse_node_control(&body).is_none());
    }

    /// Serves exactly one HTTP request on an ephemeral port and replies with the
    /// given status line + JSON body, so `send_node_heartbeat` can be exercised
    /// over a real HTTP round-trip (no external node needed).
    fn serve_once(status_line: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let url = format!("http://{}/v1/poll", listener.local_addr().unwrap());
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(&stream);
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            // Drain the request body so the client's write completes cleanly.
            let mut request_body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut request_body);
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = (&stream).write_all(response.as_bytes());
        });
        url
    }

    #[test]
    fn heartbeat_success_parses_control_over_http() {
        let url = serve_once(
            "HTTP/1.1 200 OK",
            r#"{"ok":true,"items":[],"control":{"operational_state":"enabled","directive":"continue"}}"#,
        );
        let outcome = send_node_heartbeat(&url, "adp_1", "s3cret", "lhmi_1", Some("111"), "hb-1")
            .expect("heartbeat should succeed");
        assert!(outcome.ok);
        assert_eq!(outcome.status_code, 200);
        let control = outcome.control.expect("control present");
        assert_eq!(control.directive.as_deref(), Some(directive::CONTINUE));
        assert_eq!(control.operational_state.as_deref(), Some("enabled"));
    }

    #[test]
    fn heartbeat_reject_parses_error_code_and_control_over_http() {
        // A 409 disabled reject must surface the error_code + pause directive so
        // the adapter can react instead of seeing an opaque failure.
        let url = serve_once(
            "HTTP/1.1 409 Conflict",
            r#"{"error_code":"instance_disabled","error_message":"disabled","control":{"operational_state":"disabled","directive":"pause","reason":"instance_disabled","retry_after_seconds":15}}"#,
        );
        let error = send_node_heartbeat(&url, "adp_1", "s3cret", "lhmi_1", Some("111"), "hb-1")
            .expect_err("disabled reject should be an error");
        assert_eq!(error.status_code, Some(409));
        assert_eq!(error.error_code.as_deref(), Some("instance_disabled"));
        let control = error.control.expect("control present on reject");
        assert_eq!(control.directive.as_deref(), Some(directive::PAUSE));
        assert_eq!(control.operational_state.as_deref(), Some("disabled"));
        assert_eq!(control.retry_after_seconds, Some(15));
    }
}
