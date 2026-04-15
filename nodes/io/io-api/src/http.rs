use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use io_common::frontdesk_contract::FrontdeskResultPayload;

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

pub(crate) fn frontdesk_result_response(payload: FrontdeskResultPayload) -> Response {
    let status = match (payload.status.as_str(), payload.result_code.as_str()) {
        ("ok", _) => StatusCode::OK,
        ("needs_input", _) => StatusCode::UNPROCESSABLE_ENTITY,
        ("error", "INVALID_REQUEST") => StatusCode::UNPROCESSABLE_ENTITY,
        ("error", "IDENTITY_UNAVAILABLE") => StatusCode::SERVICE_UNAVAILABLE,
        ("error", "REGISTER_FAILED") => StatusCode::BAD_GATEWAY,
        ("error", _) => StatusCode::BAD_GATEWAY,
        _ => StatusCode::OK,
    };
    (status, Json(payload)).into_response()
}

#[cfg(test)]
mod tests {
    use super::frontdesk_result_response;
    use axum::http::StatusCode;
    use io_common::frontdesk_contract::FrontdeskResultPayload;

    fn sample_payload(status: &str, result_code: &str) -> FrontdeskResultPayload {
        FrontdeskResultPayload {
            payload_type: "frontdesk_result".to_string(),
            schema_version: 1,
            status: status.to_string(),
            result_code: result_code.to_string(),
            human_message: "message".to_string(),
            missing_fields: Vec::new(),
            error_code: None,
            error_detail: None,
            ilk_id: None,
            tenant_id: Some("tnt:acme".to_string()),
            registration_status: Some("temporary".to_string()),
        }
    }

    #[test]
    fn frontdesk_needs_input_maps_to_unprocessable_entity() {
        let response = frontdesk_result_response(sample_payload(
            "needs_input",
            "MISSING_REQUIRED_FIELDS",
        ));
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn frontdesk_ok_maps_to_ok() {
        let response = frontdesk_result_response(sample_payload("ok", "REGISTERED"));
        assert_eq!(response.status(), StatusCode::OK);
    }
}
