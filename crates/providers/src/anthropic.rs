use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{error, info};

use gateway_core::error::GatewayError;
use gateway_core::provider::LLMProvider;
use gateway_core::types::*;

/// Anthropic Messages API request format.
#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<AnthropicMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    stream: bool,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

/// Anthropic Messages API response format.
#[derive(Deserialize)]
struct AnthropicResponse {
    id: String,
    #[serde(rename = "type")]
    _response_type: String,
    role: String,
    content: Vec<AnthropicContent>,
    model: String,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    message: Option<AnthropicMessageStart>,
    #[serde(default)]
    content_block: Option<AnthropicContentBlock>,
    #[serde(default)]
    delta: Option<AnthropicDelta>,
    #[serde(default)]
    usage: Option<AnthropicStreamUsage>,
}

#[derive(Deserialize)]
struct AnthropicMessageStart {
    id: String,
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    delta_type: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicStreamUsage {
    output_tokens: Option<u32>,
}

pub struct AnthropicProvider {
    name: String,
    client: Client,
    api_key: Option<String>,
    base_url: String,
    extra_headers: HashMap<String, String>,
}

impl AnthropicProvider {
    pub fn new(
        api_key: Option<String>,
        base_url: Option<String>,
        extra_headers: HashMap<String, String>,
    ) -> Self {
        Self {
            name: "anthropic".into(),
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .default_headers({
                    let mut h = reqwest::header::HeaderMap::new();
                    h.insert(
                        reqwest::header::HeaderName::from_static("anthropic-version"),
                        reqwest::header::HeaderValue::from_static("2023-06-01"),
                    );
                    h
                })
                .build()
                .expect("build client"),
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com/v1".into()),
            extra_headers,
        }
    }

    /// Build a HeaderMap from `extra_headers` (applied first), then the
    /// provider's own `x-api-key` auth on top (takes precedence).
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
                headers.insert(reqwest::header::HeaderName::from_static("x-api-key"), value);
            }
        }
        headers
    }

    /// Convert OpenAI-format request to Anthropic format.
    fn convert_request<'a>(&self, req: &'a ChatCompletionRequest) -> AnthropicRequest<'a> {
        let mut system_msg = None;
        let mut messages = Vec::new();

        for msg in &req.messages {
            if msg.role == "system" {
                if let Some(Content::Text(ref text)) = msg.content {
                    system_msg = Some(text.as_str());
                }
                continue;
            }
            let role = if msg.role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            let content = match &msg.content {
                Some(Content::Text(t)) => t.as_str(),
                Some(Content::Parts(parts)) => {
                    parts.first().and_then(|p| p.text.as_deref()).unwrap_or("")
                }
                None => "",
            };
            messages.push(AnthropicMessage { role, content });
        }

        AnthropicRequest {
            model: &req.model,
            max_tokens: req.max_tokens.unwrap_or(1024),
            system: system_msg,
            messages,
            temperature: req.temperature,
            top_p: req.top_p,
            stream: req.stream,
        }
    }

    /// Convert Anthropic response to OpenAI format.
    fn convert_response(&self, resp: AnthropicResponse) -> ChatCompletionResponse {
        let content = resp
            .content
            .iter()
            .filter(|c| c.content_type == "text")
            .filter_map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("");

        let finish_reason = resp.stop_reason.map(|r| match r.as_str() {
            "end_turn" => "stop".into(),
            "max_tokens" => "length".into(),
            "stop_sequence" => "stop".into(),
            other => other.into(),
        });

        ChatCompletionResponse {
            id: resp.id,
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp() as u64,
            model: resp.model,
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant".into(),
                    content: Some(content),
                    tool_calls: None,
                },
                finish_reason,
            }],
            usage: Usage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
            },
            system_fingerprint: None,
        }
    }
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        info!(model = %request.model, "Anthropic non-streaming request");

        let anthro_req = self.convert_request(&request);
        let url = format!("{}/messages", self.base_url);

        let resp = self
            .client
            .post(url)
            .headers(self.build_headers())
            .json(&anthro_req)
            .send()
            .await
            .map_err(|e| GatewayError::upstream(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(%status, %body, "Anthropic upstream error");
            return Err(GatewayError::upstream_status(
                status.as_u16(),
                format!("Anthropic {}: {}", status, body),
            ));
        }

        let anthro_resp = resp
            .json::<AnthropicResponse>()
            .await
            .map_err(|e| GatewayError::upstream(e.to_string()))?;

        Ok(self.convert_response(anthro_resp))
    }

    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<ChatCompletionChunk, GatewayError>>,
        GatewayError,
    > {
        let mut anthro_req = self.convert_request(&request);
        anthro_req.stream = true;

        info!(model = %request.model, "Anthropic streaming request");

        let url = format!("{}/messages", self.base_url);
        let resp = self
            .client
            .post(url)
            .headers(self.build_headers())
            .json(&anthro_req)
            .send()
            .await
            .map_err(|e| GatewayError::upstream(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::upstream_status(
                status.as_u16(),
                format!("Anthropic {}: {}", status, body),
            ));
        }

        let stream = resp.bytes_stream();
        let message_id = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let model = request.model.clone();

        let chunks = stream.filter_map(move |item| {
            let model = model.clone();
            let message_id = message_id.clone();
            async move {
                match item {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let mut results = Vec::new();

                        for line in text.lines() {
                            let line = line.trim();
                            if line.starts_with("event: ") {
                                continue;
                            }
                            if line.starts_with("data: ") {
                                let data = &line[6..];
                                if let Ok(event) =
                                    serde_json::from_str::<AnthropicStreamEvent>(data)
                                {
                                    match event.event_type.as_str() {
                                        "message_start" => {
                                            if let Some(ref msg) = event.message {
                                                *message_id.lock().unwrap() = msg.id.clone();
                                            }
                                        }
                                        "content_block_delta" => {
                                            if let Some(ref delta) = event.delta {
                                                if let Some(ref text) = delta.text {
                                                    let id = message_id.lock().unwrap().clone();
                                                    results.push(Ok(
                                                        ChatCompletionChunk::new_delta(
                                                            &model,
                                                            &id,
                                                            Some(text.clone()),
                                                            None,
                                                        ),
                                                    ));
                                                }
                                            }
                                        }
                                        "message_delta" => {
                                            // End of message
                                        }
                                        "message_stop" => {
                                            let id = message_id.lock().unwrap().clone();
                                            results.push(Ok(ChatCompletionChunk::new_delta(
                                                &model,
                                                &id,
                                                None,
                                                Some("stop".into()),
                                            )));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }

                        if results.is_empty() {
                            None
                        } else {
                            Some(results.into_iter().next().unwrap())
                        }
                    }
                    Err(e) => Some(Err(GatewayError::upstream(e.to_string()))),
                }
            }
        });

        Ok(chunks.boxed())
    }
}
