//! The error type every handler returns: an HTTP status plus `{"error":{"code","message",..}}`.

use crate::model::Holder;
use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub holder: Option<Holder>,
    pub current_version: Option<u64>,
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError { status, code, message: message.into(), holder: None, current_version: None, retry_after: None }
    }

    pub fn unauthorized(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    pub fn forbidden(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub fn not_found(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn locked(holder: Holder) -> ApiError {
        let mut e = ApiError::new(StatusCode::CONFLICT, "locked", format!("profile is open: {holder}"));
        e.holder = Some(holder);
        e
    }

    pub fn stale(current_version: u64) -> ApiError {
        let mut e = ApiError::new(
            StatusCode::CONFLICT,
            "stale",
            format!("base_version does not match the stored data (current version is {current_version})"),
        );
        e.current_version = Some(current_version);
        e
    }

    pub fn invalid(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid", message)
    }

    pub fn too_large(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message)
    }

    pub fn lease_required() -> ApiError {
        ApiError::new(StatusCode::FORBIDDEN, "lease_required", "this needs the lease of the current lock holder (X-Citro-Lease)")
    }

    pub fn lease_invalid() -> ApiError {
        ApiError::new(StatusCode::FORBIDDEN, "lease_invalid", "the lease is wrong, expired or was taken over")
    }

    pub fn internal(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    /// Too many failed logins from one address (the one error code beyond the contract's list).
    pub fn rate_limited(retry_after_secs: u64) -> ApiError {
        ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many failed logins; try again later")
            .with_retry_after(retry_after_secs.max(1))
    }

    pub fn with_retry_after(mut self, secs: u64) -> ApiError {
        self.retry_after = Some(secs);
        self
    }

    pub fn body(&self) -> Value {
        let mut err = json!({"code": self.code, "message": self.message});
        if let Some(h) = &self.holder {
            err["holder"] = json!(h);
        }
        if let Some(v) = self.current_version {
            err["current_version"] = json!(v);
        }
        json!({ "error": err })
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.status.as_u16(), self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut resp = (self.status, Json(self.body())).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            resp.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(secs) = self.retry_after {
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        resp
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> ApiError {
        tracing::error!("i/o error while serving a request: {e}");
        ApiError::internal("storage error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_carries_the_holder_and_nothing_secret() {
        let e = ApiError::locked(Holder { user: "ann".into(), device_name: "Laptop".into(), since: 5, expires_at: 95 });
        assert_eq!(e.status, StatusCode::CONFLICT);
        let b = e.body();
        assert_eq!(b["error"]["code"], "locked");
        assert_eq!(b["error"]["holder"]["device_name"], "Laptop");
        assert!(b["error"]["holder"].get("device_id").is_none());
    }

    #[test]
    fn status_codes_match_the_contract() {
        let cases = [
            (ApiError::unauthorized("x"), 401, "unauthorized"),
            (ApiError::forbidden("x"), 403, "forbidden"),
            (ApiError::not_found("x"), 404, "not_found"),
            (ApiError::stale(3), 409, "stale"),
            (ApiError::invalid("x"), 400, "invalid"),
            (ApiError::too_large("x"), 413, "too_large"),
            (ApiError::lease_required(), 403, "lease_required"),
            (ApiError::lease_invalid(), 403, "lease_invalid"),
            (ApiError::rate_limited(5), 429, "rate_limited"),
            (ApiError::internal("x"), 500, "internal"),
        ];
        for (e, status, code) in cases {
            assert_eq!((e.status.as_u16(), e.code), (status, code));
        }
        assert_eq!(ApiError::stale(3).body()["error"]["current_version"], 3);
    }
}
