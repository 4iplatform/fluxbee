use std::fmt::{Display, Formatter};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;

/// Header the IO.linkedhelper node uses to identify the calling adapter.
const ADAPTER_ID_HEADER: &str = "X-Fluxbee-Adapter-Id";

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
}

/**
 * Structured error returned when reporting to the node fails (transport or
 * non-2xx). Never aborts the adapter cycle; it is surfaced per binding.
 */
#[derive(Debug)]
pub struct NodeReportError {
    pub status_code: Option<u16>,
    pub message: String,
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
        })?;

    let status_code = response.status().as_u16();
    if !response.status().is_success() {
        let message = response
            .text()
            .unwrap_or_else(|_| String::from("unknown node error"));
        return Err(NodeReportError {
            status_code: Some(status_code),
            message,
        });
    }

    let parsed: serde_json::Value = response.json().map_err(|error| NodeReportError {
        status_code: Some(status_code),
        message: format!("failed to parse node response: {}", error),
    })?;
    let ok = parsed.get("ok").and_then(|value| value.as_bool()).unwrap_or(false);

    Ok(NodeHeartbeatOutcome { status_code, ok })
}
