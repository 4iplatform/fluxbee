use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FrontdeskHttpEnvelope {
    pub success: bool,
    pub human_message: String,
    #[serde(default)]
    pub error_code: Option<String>,
}

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

pub(crate) fn frontdesk_result_response(payload: FrontdeskHttpEnvelope) -> Response {
    let status = match (payload.success, payload.error_code.as_deref()) {
        (true, _) => StatusCode::OK,
        (false, Some("missing_required_fields")) => StatusCode::UNPROCESSABLE_ENTITY,
        (false, Some("invalid_request")) => StatusCode::UNPROCESSABLE_ENTITY,
        (false, Some("identity_unavailable")) => StatusCode::SERVICE_UNAVAILABLE,
        (false, Some("register_failed")) => StatusCode::BAD_GATEWAY,
        (false, Some(_)) => StatusCode::BAD_GATEWAY,
        (false, None) => StatusCode::BAD_GATEWAY,
    };
    (status, Json(payload)).into_response()
}

#[cfg(test)]
mod tests {
    use super::{frontdesk_result_response, FrontdeskHttpEnvelope};
    use axum::http::StatusCode;

    fn sample_payload(success: bool, error_code: Option<&str>) -> FrontdeskHttpEnvelope {
        FrontdeskHttpEnvelope {
            success,
            human_message: "message".to_string(),
            error_code: error_code.map(ToString::to_string),
        }
    }

    #[test]
    fn frontdesk_needs_input_maps_to_unprocessable_entity() {
        let response = frontdesk_result_response(sample_payload(
            false,
            Some("missing_required_fields"),
        ));
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn frontdesk_ok_maps_to_ok() {
        let response = frontdesk_result_response(sample_payload(true, None));
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn frontdesk_error_invalid_request_maps_to_unprocessable_entity() {
        let response =
            frontdesk_result_response(sample_payload(false, Some("invalid_request")));
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn frontdesk_error_identity_unavailable_maps_to_service_unavailable() {
        let response = frontdesk_result_response(sample_payload(
            false,
            Some("identity_unavailable"),
        ));
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn frontdesk_error_register_failed_maps_to_bad_gateway() {
        let response =
            frontdesk_result_response(sample_payload(false, Some("register_failed")));
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
}
