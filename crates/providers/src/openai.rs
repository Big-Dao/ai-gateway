use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Client;
use std::collections::HashMap;
use tracing::{error, info};

use gateway_core::error::GatewayError;
use gateway_core::provider::LLMProvider;
use gateway_core::types::*;

pub struct OpenAIProvider {
    name: String,
    client: Client,
    api_key: Option<String>,
    base_url: String,
    extra_headers: HashMap<String, String>,
}

impl OpenAIProvider {
    pub fn new(
        api_key: Option<String>,
        base_url: Option<String>,
        extra_headers: HashMap<String, String>,
    ) -> Self {
        Self {
            name: "openai".into(),
            client: Client::new(),
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            extra_headers,
        }
    }

    /// Build a HeaderMap from `extra_headers` (applied first), then the
    /// provider's own Authorization header on top (takes precedence).
    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (k, v) in &self.extra_headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                headers.insert(name, value);
            }
        }
        if let Some(key) = &self.api_key {
            if let Ok(value) = HeaderValue::from_str(key) {
                headers.insert(reqwest::header::AUTHORIZATION, value);
            }
        }
        headers
    }

    fn build_client(&self) -> reqwest::RequestBuilder {
        let url = format!("{}/chat/completions", self.base_url);
        self.client.post(url).headers(self.build_headers())
    }
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        info!(model = %request.model, "OpenAI non-streaming request");

        let resp = self
            .build_client()
            .json(&request)
            .send()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(%status, %body, "OpenAI upstream error");
            return Err(GatewayError::UpstreamError(format!(
                "OpenAI {}: {}",
                status, body
            )));
        }

        resp.json::<ChatCompletionResponse>()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))
    }

    async fn chat_completion_stream(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<ChatCompletionChunk, GatewayError>>,
        GatewayError,
    > {
        request.stream = true;
        info!(model = %request.model, "OpenAI streaming request");

        let resp = self
            .build_client()
            .json(&request)
            .send()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::UpstreamError(format!(
                "OpenAI {}: {}",
                status, body
            )));
        }

        let stream = resp.bytes_stream();
        let chunks = stream.filter_map(|item| async move {
            match item {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        let line = line.trim();
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                return None;
                            }
                            match serde_json::from_str::<ChatCompletionChunk>(data) {
                                Ok(chunk) => return Some(Ok(chunk)),
                                Err(e) => {
                                    return Some(Err(GatewayError::UpstreamError(format!(
                                        "SSE parse error: {}",
                                        e
                                    ))));
                                }
                            }
                        }
                    }
                    None
                }
                Err(e) => Some(Err(GatewayError::UpstreamError(e.to_string()))),
            }
        });

        Ok(chunks.boxed())
    }
}
