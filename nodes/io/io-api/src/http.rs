use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub(crate) fn accepted_response(
    node_name: &str,
    request_id: String,
    trace_id: Option<String>,
    relay_status: &str,
) -> Response {
    tracing::debug!(
        node_name = %node_name,
        request_id = %request_id,
        trace_id = ?trace_id,
        relay_status = %relay_status,
        "io-api request accepted"
    );
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "accepted",
            "request_id": request_id,
            "trace_id": trace_id,
            "relay_status": relay_status,
            "node_name": node_name,
        })),
    )
        .into_response()
}

pub(crate) fn api_error(
    status: StatusCode,
    error_code: &'static str,
    error_message: impl Into<String>,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "status": "error",
            "error_code": error_code,
            "error_message": error_message.into(),
        })),
    )
        .into_response()
}
