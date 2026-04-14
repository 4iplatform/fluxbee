use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

pub(crate) fn accepted_response(
    node_name: &str,
    request_id: String,
    trace_id: Option<String>,
    relay_status: &str,
    ilk: Option<&str>,
) -> Response {
    tracing::debug!(
        node_name = %node_name,
        request_id = %request_id,
        trace_id = ?trace_id,
        relay_status = %relay_status,
        ilk = ?ilk,
        "io-api request accepted"
    );
    let mut body = serde_json::json!({
        "status": "accepted",
        "request_id": request_id,
        "trace_id": trace_id,
        "relay_status": relay_status,
        "node_name": node_name,
    });
    if let Some(ilk) = ilk {
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "ilk".to_string(),
                serde_json::Value::String(ilk.to_string()),
            );
        }
    }
    (StatusCode::ACCEPTED, Json(body)).into_response()
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
