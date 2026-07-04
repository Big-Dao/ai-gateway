use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, instrument, warn};

use gateway_core::error::GatewayError;
use gateway_core::types::*;

use crate::circuit_breaker::CircuitBreaker;
use crate::state::AppState;

/// Retry configuration.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (not counting the initial attempt).
    pub max_retries: u32,
    /// Initial backoff duration.
    pub initial_backoff: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
    /// Backoff multiplier.
    pub backoff_multiplier: f64,
    /// Whether to attempt fallback to another provider on final failure.
    #[allow(dead_code)] // reserved for a per-request fallback toggle (currently always-on)
    pub enable_fallback: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 2,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(16),
            backoff_multiplier: 2.0,
            enable_fallback: true,
        }
    }
}

/// Attempt information for logging/observability.
#[derive(Debug, Clone)]
pub struct AttemptInfo {
    pub provider_name: String,
    pub attempt_number: u32,
    pub is_fallback: bool,
}

/// Execute a non-streaming chat completion with retry and fallback.
#[instrument(skip(state, circuit_breaker, config, request), fields(model = %request.model))]
pub async fn chat_completion_with_retry(
    state: &AppState,
    circuit_breaker: &CircuitBreaker,
    config: &RetryConfig,
    request: ChatCompletionRequest,
) -> Result<(ChatCompletionResponse, AttemptInfo), GatewayError> {
    let model = request.model.clone();

    // Build the ordered list of providers to try.
    let provider_names = build_fallback_chain(state, &model).await;

    if provider_names.is_empty() {
        return Err(GatewayError::ProviderNotFound(model));
    }

    let mut last_error = None;
    let mut attempt_info = AttemptInfo {
        provider_name: String::new(),
        attempt_number: 0,
        is_fallback: false,
    };

    for (chain_idx, provider_name) in provider_names.iter().enumerate() {
        // Check circuit breaker.
        if !circuit_breaker.allow_request(provider_name).await {
            warn!(
                provider = provider_name,
                "Circuit breaker open — skipping provider"
            );
            continue;
        }

        let is_fallback = chain_idx > 0;
        attempt_info.provider_name = provider_name.clone();
        attempt_info.is_fallback = is_fallback;

        // Get the provider instance by name (not model) so fallback works.
        let provider = match state.get_provider_by_name(provider_name).await {
            Some(p) => p,
            None => continue,
        };

        // Retry loop for this provider.
        let mut backoff = config.initial_backoff;
        for attempt in 0..=config.max_retries {
            attempt_info.attempt_number = attempt + 1;

            if attempt > 0 {
                info!(
                    provider = provider_name,
                    attempt = attempt + 1,
                    backoff_ms = backoff.as_millis(),
                    "Retrying request after backoff"
                );
                sleep(backoff).await;
                backoff = std::cmp::min(
                    Duration::from_secs_f64(backoff.as_secs_f64() * config.backoff_multiplier),
                    config.max_backoff,
                );
            }

            match provider.chat_completion(request.clone()).await {
                Ok(response) => {
                    circuit_breaker.record_success(provider_name).await;
                    if is_fallback {
                        info!(
                            provider = provider_name,
                            original_model = model,
                            "Fallback provider succeeded"
                        );
                    }
                    return Ok((response, attempt_info));
                }
                Err(e) => {
                    let should_retry = is_retryable(&e);
                    warn!(
                        provider = provider_name,
                        attempt = attempt + 1,
                        error = %e,
                        retryable = should_retry,
                        "Provider request failed"
                    );

                    if !should_retry || attempt == config.max_retries {
                        circuit_breaker.record_failure(provider_name).await;
                        last_error = Some(e);
                        break;
                    }
                }
            }
        }
    }

    Err(last_error.unwrap_or(GatewayError::ProviderNotFound(model)))
}

/// Execute a streaming chat completion with retry and fallback.
#[instrument(skip(state, circuit_breaker, _config, request), fields(model = %request.model))]
pub async fn chat_completion_stream_with_retry(
    state: &AppState,
    circuit_breaker: &CircuitBreaker,
    _config: &RetryConfig,
    request: ChatCompletionRequest,
) -> Result<(gateway_core::provider::ChunkStream, AttemptInfo), GatewayError> {
    let model = request.model.clone();
    let provider_names = build_fallback_chain(state, &model).await;

    if provider_names.is_empty() {
        return Err(GatewayError::ProviderNotFound(model));
    }

    let mut last_error = None;
    let mut attempt_info = AttemptInfo {
        provider_name: String::new(),
        attempt_number: 0,
        is_fallback: false,
    };

    for (chain_idx, provider_name) in provider_names.iter().enumerate() {
        if !circuit_breaker.allow_request(provider_name).await {
            warn!(
                provider = provider_name,
                "Circuit breaker open — skipping provider (streaming)"
            );
            continue;
        }

        let is_fallback = chain_idx > 0;
        attempt_info.provider_name = provider_name.clone();
        attempt_info.is_fallback = is_fallback;

        let provider = match state.get_provider_by_name(provider_name).await {
            Some(p) => p,
            None => continue,
        };

        // For streaming, we don't do multi-attempt retry within a provider
        // because the stream has already started. Instead, we try once
        // and fall back to the next provider on failure.
        match provider.chat_completion_stream(request.clone()).await {
            Ok(stream) => {
                circuit_breaker.record_success(provider_name).await;
                return Ok((stream, attempt_info));
            }
            Err(e) => {
                warn!(
                    provider = provider_name,
                    error = %e,
                    "Streaming request failed"
                );
                circuit_breaker.record_failure(provider_name).await;
                last_error = Some(e);
                // Try next provider immediately for streaming.
                continue;
            }
        }
    }

    Err(last_error.unwrap_or(GatewayError::ProviderNotFound(model)))
}

/// Build the ordered list of provider names to try for a given model.
/// Returns providers that serve this model, ordered by priority.
async fn build_fallback_chain(state: &AppState, model: &str) -> Vec<String> {
    let config = state.config.read().await;

    // Find all provider names that serve this model.
    let mut providers: Vec<String> = config
        .providers
        .iter()
        .filter(|(_, cfg)| cfg.models.contains(&model.to_string()))
        .map(|(name, _)| name.clone())
        .collect();

    // Sort: built-in providers first, then custom ones.
    // This ensures well-tested adapters are tried first.
    let builtins = ["openai", "anthropic", "gemini", "ollama"];
    providers.sort_by_key(|name| {
        builtins
            .iter()
            .position(|b| *b == name.as_str())
            .unwrap_or(999)
    });

    providers
}

/// Determine if an error is retryable.
fn is_retryable(error: &GatewayError) -> bool {
    match error {
        // Timeouts and upstream errors are retryable.
        GatewayError::UpstreamError(msg) => {
            // Don't retry client errors (4xx except 429).
            // NOTE: status codes are matched on the error string; see known-issues —
            // this should move to a structured status field.
            !(msg.contains("400")
                || msg.contains("401")
                || msg.contains("403")
                || msg.contains("404"))
        }
        // Rate limited — retryable with backoff.
        GatewayError::RateLimited => true,
        // Provider not found — not retryable.
        GatewayError::ProviderNotFound(_) => false,
        // Auth errors — not retryable.
        GatewayError::AuthenticationFailed(_) => false,
        // Bad request — not retryable.
        GatewayError::BadRequest(_) => false,
        // Everything else — retryable.
        _ => true,
    }
}
