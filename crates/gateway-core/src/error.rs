use crate::types::ErrorResponse;

#[derive(thiserror::Error, Debug)]
pub enum GatewayError {
    #[error("Provider not found for model: {0}")]
    ProviderNotFound(String),

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Insufficient permissions: {0}")]
    Forbidden(String),

    #[error("Rate limit exceeded")]
    RateLimited,

    #[error("Quota exceeded: {limit_type} limit={limit}, current={current}")]
    QuotaExceeded {
        limit_type: String,
        limit: u64,
        current: u64,
    },

    #[error("Upstream provider error: {0}")]
    UpstreamError(String),

    #[error("Invalid request: {0}")]
    BadRequest(String),

    #[error("Cache error: {0}")]
    CacheError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl GatewayError {
    /// Returns the appropriate HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        use http::StatusCode;
        match self {
            GatewayError::ProviderNotFound(_) => StatusCode::NOT_FOUND.as_u16(),
            GatewayError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED.as_u16(),
            GatewayError::Forbidden(_) => StatusCode::FORBIDDEN.as_u16(),
            GatewayError::RateLimited => StatusCode::TOO_MANY_REQUESTS.as_u16(),
            GatewayError::QuotaExceeded { .. } => StatusCode::TOO_MANY_REQUESTS.as_u16(),
            GatewayError::UpstreamError(_) => StatusCode::BAD_GATEWAY.as_u16(),
            GatewayError::BadRequest(_) => StatusCode::BAD_REQUEST.as_u16(),
            GatewayError::CacheError(_) => StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
            GatewayError::ConfigError(_) => StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
            GatewayError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
        }
    }

    /// Returns the OpenAI-compatible error type string.
    pub fn error_type(&self) -> &'static str {
        match self {
            GatewayError::ProviderNotFound(_) => "model_not_found",
            GatewayError::AuthenticationFailed(_) => "authentication_error",
            GatewayError::Forbidden(_) => "insufficient_permissions",
            GatewayError::RateLimited => "rate_limit_exceeded",
            GatewayError::QuotaExceeded { .. } => "quota_exceeded",
            GatewayError::UpstreamError(_) => "upstream_error",
            GatewayError::BadRequest(_) => "invalid_request_error",
            GatewayError::CacheError(_) => "cache_error",
            GatewayError::ConfigError(_) => "config_error",
            GatewayError::Internal(_) => "internal_error",
        }
    }

    /// Build an OpenAI-format error response body.
    pub fn to_error_response(&self) -> ErrorResponse {
        let code = match self {
            GatewayError::QuotaExceeded { limit_type, .. } => Some(format!("quota_exceeded:{}", limit_type)),
            GatewayError::RateLimited => Some("rate_limit_exceeded".into()),
            GatewayError::AuthenticationFailed(_) => Some("authentication_error".into()),
            GatewayError::Forbidden(_) => Some("insufficient_permissions".into()),
            _ => None,
        };
        ErrorResponse {
            error: crate::types::ErrorDetail {
                message: self.to_string(),
                error_type: self.error_type().into(),
                param: None,
                code,
            },
        }
    }
}
