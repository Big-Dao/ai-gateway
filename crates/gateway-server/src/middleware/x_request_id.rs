//! Propagates `X-Request-Id` between client and gateway.
//!
//! Two responsibilities:
//! 1.  Pull `X-Request-Id` off the incoming request (or generate a UUIDv4 if
//!     absent) and attach it to the response headers so callers can correlate
//!     error responses with gateways logs.
//! 2.  Attach a `Retry-After` hint to `429 Too Many Requests` and
//!     `503 Service Unavailable` responses that did not already set one.

use axum::{
    extract::Request,
    http::{header, HeaderValue},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

/// Extension key for handing the request id into inner handlers if needed.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

pub async fn x_request_id_middleware(request: Request, next: Next) -> Response {
    let req_id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut resp = next.run(request).await;

    if let Ok(v) = HeaderValue::from_str(&req_id) {
        resp.headers_mut().insert(header::HeaderName::from_static("x-request-id"), v);
    }

    let status = resp.status();
    if (status.is_client_error() || status.is_server_error())
        && (status.as_u16() == 429 || status.as_u16() == 503)
        && !resp.headers().contains_key(header::RETRY_AFTER)
    {
        let secs: &'static str = if status.as_u16() == 429 {
            "60"
        } else {
            "30"
        };
        // Safe: static digits
        let v = HeaderValue::from_static(secs);
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }

    resp
}
