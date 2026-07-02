use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};
use std::time::Duration;

/**
 * Serialized enroll request expected by Fluxbee Cloud.
 */
#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterEnrollRequest {
    #[serde(rename = "enrollmentToken")]
    pub enrollment_token: String,
    #[serde(rename = "adapterType")]
    pub adapter_type: String,
    #[serde(rename = "displayName", skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(rename = "deviceHint", skip_serializing_if = "Option::is_none")]
    pub device_hint: Option<String>,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterEnrollResponse {
    pub result: AdapterEnrollResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterEnrollResult {
    #[serde(rename = "adapterId")]
    pub adapter_id: String,
    #[serde(rename = "adapterSecret")]
    pub adapter_secret: String,
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "syncConfig")]
    pub sync_config: AdapterSyncConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterSyncConfig {
    #[serde(rename = "cloudBaseUrl")]
    pub cloud_base_url: String,
    #[serde(rename = "discoveryUrl")]
    pub discovery_url: String,
    #[serde(rename = "syncUrl")]
    pub sync_url: Option<String>,
    #[serde(rename = "reportTo")]
    pub report_to: Option<String>,
}

/**
 * Serialized discovery request expected by Fluxbee Cloud.
 */
#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterDiscoveryRequest {
    #[serde(rename = "adapterType")]
    pub adapter_type: String,
    pub instances: Vec<AdapterDiscoveryRequestItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterDiscoveryRequestItem {
    #[serde(rename = "localInstanceId")]
    pub local_instance_id: String,
    #[serde(rename = "localPath", skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(rename = "accountFingerprint", skip_serializing_if = "Option::is_none")]
    pub account_fingerprint: Option<String>,
    #[serde(rename = "accountHint", skip_serializing_if = "Option::is_none")]
    pub account_hint: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterDiscoveryResponse {
    pub result: AdapterDiscoveryResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterDiscoveryResult {
    pub received: usize,
    pub items: Vec<AdapterDiscoveryResponseItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterDiscoveryResponseItem {
    #[serde(rename = "discoveredInstanceId")]
    pub discovered_instance_id: String,
    #[serde(rename = "localInstanceId")]
    pub local_instance_id: String,
    pub status: String,
    #[serde(rename = "managedInstanceId")]
    pub managed_instance_id: Option<String>,
    #[serde(rename = "reportTo")]
    pub report_to: Option<String>,
}

/**
 * Serialized alive request expected by Fluxbee Cloud.
 */
#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterAliveRequest {
    #[serde(rename = "adapterType")]
    pub adapter_type: String,
    #[serde(rename = "adapterVersion")]
    pub adapter_version: String,
    #[serde(rename = "adapterBuild")]
    pub adapter_build: String,
    pub os: String,
    pub arch: String,
    pub status: String,
    #[serde(rename = "reportedAt")]
    pub reported_at: String,
    #[serde(rename = "lastKnownDesiredStateVersion", skip_serializing_if = "Option::is_none")]
    pub last_known_desired_state_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<AdapterAliveServicePayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linkedhelper: Option<AdapterAliveLinkedHelperPayload>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterAliveServicePayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(rename = "uptimeSeconds", skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
    #[serde(
        rename = "lastSuccessfulDiscoveryAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_successful_discovery_at: Option<String>,
    #[serde(rename = "lastErrorCode", skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(rename = "lastErrorMessage", skip_serializing_if = "Option::is_none")]
    pub last_error_message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterAliveLinkedHelperPayload {
    #[serde(rename = "lhRootStatus", skip_serializing_if = "Option::is_none")]
    pub lh_root_status: Option<String>,
    #[serde(rename = "lhRootPath", skip_serializing_if = "Option::is_none")]
    pub lh_root_path: Option<String>,
    #[serde(rename = "instancesCount", skip_serializing_if = "Option::is_none")]
    pub instances_count: Option<usize>,
    #[serde(rename = "schemaSignature", skip_serializing_if = "Option::is_none")]
    pub schema_signature: Option<String>,
    #[serde(rename = "compatibilityStatus", skip_serializing_if = "Option::is_none")]
    pub compatibility_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterAliveResponse {
    pub ok: bool,
    #[serde(rename = "serverTime")]
    pub server_time: String,
    #[serde(rename = "adapterStatus")]
    pub adapter_status: String,
    #[serde(rename = "desiredStateChanged")]
    pub desired_state_changed: bool,
    #[serde(rename = "desiredStateVersion")]
    pub desired_state_version: u64,
    #[serde(rename = "desiredState")]
    pub desired_state: Option<AdapterAliveDesiredState>,
    pub commands: Vec<Value>,
    #[serde(default)]
    pub update: AdapterUpdateDirective,
    pub compatibility: AdapterAliveCompatibilityResponse,
}

/**
 * Cloud's per-alive update decision. `required` means the reported adapter
 * version is below the minimum supported version and should be applied on the
 * next safe boundary; `available` is a non-mandatory upgrade offer. `target`
 * carries the concrete artifact to install and is present when either is true.
 */
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdapterUpdateDirective {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub target: Option<AdapterUpdateTarget>,
}

/**
 * A downloadable adapter release. `url` may be absolute or a path relative to
 * the adapter's configured Cloud base URL. `sha256` is the mandatory integrity
 * check performed before swapping the binary; `sig` is an optional detached
 * signature reserved for the signing pipeline (not yet produced).
 */
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterUpdateTarget {
    #[serde(rename = "releaseId")]
    pub release_id: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub sig: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterAliveDesiredState {
    pub bindings: Vec<AdapterAliveDesiredBinding>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterAliveDesiredBinding {
    #[serde(rename = "localInstanceId")]
    pub local_instance_id: String,
    #[serde(rename = "managedInstanceId")]
    pub managed_instance_id: String,
    pub status: String,
    #[serde(rename = "reportTo", default)]
    pub report_to: Option<AdapterReportTo>,
}

/**
 * Where the adapter must report events for one active binding. In the
 * intermediate (no-Edge) path `kind` is `direct_node_http` and `url` points at
 * the IO.linkedhelper node's `/v1/poll`; `auth` is always `adapter_secret`.
 */
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterReportTo {
    pub kind: String,
    pub url: String,
    #[serde(rename = "routeId", default)]
    pub route_id: Option<String>,
    #[serde(default)]
    pub auth: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterAliveCompatibilityResponse {
    pub status: String,
    pub decision: String,
}

/**
 * Structured HTTP error returned by Cloud adapter endpoints.
 */
#[derive(Debug)]
pub struct AdapterHttpError {
    pub operation: &'static str,
    pub status_code: Option<u16>,
    pub response_body: String,
}

impl AdapterHttpError {
    pub fn is_auth_error(&self) -> bool {
        matches!(self.status_code, Some(401 | 403))
    }
}

impl Display for AdapterHttpError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.status_code {
            Some(status_code) => write!(
                f,
                "Cloud {} request failed with status {}: {}",
                self.operation, status_code, self.response_body
            ),
            None => write!(
                f,
                "Cloud {} request failed: {}",
                self.operation, self.response_body
            ),
        }
    }
}

impl std::error::Error for AdapterHttpError {}

/**
 * Calls the Cloud enrollment endpoint and returns the typed response.
 */
pub fn enroll_adapter(
    cloud_base_url: &str,
    request: &AdapterEnrollRequest,
) -> Result<AdapterEnrollResponse, AdapterHttpError> {
    let url = format!("{}/api/adapters/enroll", cloud_base_url.trim_end_matches('/'));
    let client = build_http_client().map_err(io_error_to_http("enroll"))?;
    let response = client
        .post(url)
        .header(CONTENT_TYPE, "application/json")
        .json(request)
        .send()
        .map_err(io_error_to_http("enroll"))?;

    parse_json_response("enroll", response)
}

/**
 * Calls the Cloud discovery endpoint with the stored adapter credentials.
 */
pub fn send_discovery(
    discovery_url: &str,
    adapter_secret: &str,
    request: &AdapterDiscoveryRequest,
) -> Result<AdapterDiscoveryResponse, AdapterHttpError> {
    let client = build_http_client().map_err(io_error_to_http("discovery"))?;
    let response = client
        .post(discovery_url)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {}", adapter_secret))
        .json(request)
        .send()
        .map_err(io_error_to_http("discovery"))?;

    parse_json_response("discovery", response)
}

/**
 * Calls the Cloud alive endpoint with the stored adapter credentials.
 */
pub fn send_alive(
    alive_url: &str,
    adapter_secret: &str,
    request: &AdapterAliveRequest,
) -> Result<AdapterAliveResponse, AdapterHttpError> {
    let client = build_http_client().map_err(io_error_to_http("alive"))?;
    let response = client
        .post(alive_url)
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, format!("Bearer {}", adapter_secret))
        .json(request)
        .send()
        .map_err(io_error_to_http("alive"))?;

    parse_json_response("alive", response)
}

/**
 * Builds one shared HTTP client with conservative defaults for the current adapter phase.
 */
fn build_http_client() -> Result<Client, reqwest::Error> {
    Client::builder().timeout(Duration::from_secs(20)).build()
}

fn parse_json_response<T: DeserializeOwned>(
    operation: &'static str,
    response: Response,
) -> Result<T, AdapterHttpError> {
    let status_code = response.status().as_u16();
    if !response.status().is_success() {
        let body = response
            .text()
            .unwrap_or_else(|_| String::from("Unknown Cloud error"));
        return Err(AdapterHttpError {
            operation,
            status_code: Some(status_code),
            response_body: body,
        });
    }

    response.json().map_err(|error| AdapterHttpError {
        operation,
        status_code: Some(status_code),
        response_body: format!("Failed to parse Cloud JSON response: {}", error),
    })
}

fn io_error_to_http(
    operation: &'static str,
) -> impl FnOnce(reqwest::Error) -> AdapterHttpError {
    move |error| AdapterHttpError {
        operation,
        status_code: error.status().map(|value| value.as_u16()),
        response_body: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alive_response_parses_full_update_directive() {
        let json = serde_json::json!({
            "ok": true,
            "serverTime": "2026-07-02T00:00:00Z",
            "adapterStatus": "accepted",
            "desiredStateChanged": false,
            "desiredStateVersion": 3,
            "desiredState": null,
            "commands": [],
            "update": {
                "available": true,
                "required": true,
                "reason": "below minimum",
                "target": {
                    "releaseId": "lh-adapter-0.2.0-linux-x64",
                    "version": "0.2.0",
                    "url": "/api/adapters/a/artifacts/lh-adapter-0.2.0-linux-x64",
                    "sha256": "abc123",
                    "size": 42,
                    "sig": null
                }
            },
            "compatibility": { "status": "compatible", "decision": "allow" }
        });

        let parsed: AdapterAliveResponse = serde_json::from_value(json).unwrap();
        assert!(parsed.update.available);
        assert!(parsed.update.required);
        let target = parsed.update.target.expect("target present");
        assert_eq!(target.version, "0.2.0");
        assert_eq!(target.size, 42);
        assert!(target.sig.is_none());
    }

    #[test]
    fn alive_response_defaults_update_when_absent() {
        // An older/leaner Cloud response without `update` must still parse and
        // yield a benign "no update" directive.
        let json = serde_json::json!({
            "ok": true,
            "serverTime": "2026-07-02T00:00:00Z",
            "adapterStatus": "accepted",
            "desiredStateChanged": false,
            "desiredStateVersion": 0,
            "desiredState": null,
            "commands": [],
            "compatibility": { "status": "unknown", "decision": "allow" }
        });

        let parsed: AdapterAliveResponse = serde_json::from_value(json).unwrap();
        assert!(!parsed.update.available);
        assert!(!parsed.update.required);
        assert!(parsed.update.target.is_none());
    }
}
