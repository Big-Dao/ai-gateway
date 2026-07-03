use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, error};

use gateway_core::error::GatewayError;
use gateway_core::provider::LLMProvider;
use gateway_core::types::*;

#[derive(Serialize)]
struct OllamaRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
}

#[derive(Serialize)]
struct OllamaMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Deserialize)]
struct OllamaResponse {
    model: String,
    message: OllamaResponseMessage,
    done: bool,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

#[derive(Deserialize)]
struct OllamaResponseMessage {
    role: String,
    content: String,
}

pub struct OllamaProvider {
    name: String,
    client: Client,
    base_url: String,
    extra_headers: HashMap<String, String>,
}

impl OllamaProvider {
    pub fn new(
        _api_key: Option<String>,
        base_url: Option<String>,
        extra_headers: HashMap<String, String>,
    ) -> Self {
        Self {
            name: "ollama".into(),
            client: Client::new(),
            base_url: base_url.unwrap_or_else(|| "http://localhost:11434".into()),
            extra_headers,
        }
    }

    /// Build a HeaderMap from `extra_headers`. Ollama takes no auth header.
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
        headers
    }

    fn convert_request<'a>(&self, req: &'a ChatCompletionRequest) -> OllamaRequest<'a> {
        let messages: Vec<OllamaMessage<'_>> = req
            .messages
            .iter()
            .map(|msg| {
                let content = match &msg.content {
                    Some(Content::Text(t)) => t.as_str(),
                    Some(Content::Parts(parts)) => {
                        parts.first().and_then(|p| p.text.as_deref()).unwrap_or("")
                    }
                    None => "",
                };
                OllamaMessage {
                    role: &msg.role,
                    content,
                }
            })
            .collect();

        OllamaRequest {
            model: &req.model,
            messages,
            stream: req.stream,
            options: Some(OllamaOptions {
                temperature: req.temperature,
                num_predict: req.max_tokens,
                top_p: req.top_p,
            }),
        }
    }
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        info!(model = %request.model, "Ollama non-streaming request");

        let ollama_req = self.convert_request(&request);
        let url = format!("{}/api/chat", self.base_url);

        let resp = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&ollama_req)
            .send()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(%status, %body, "Ollama upstream error");
            return Err(GatewayError::UpstreamError(format!("Ollama {}: {}", status, body)));
        }

        let ollama_resp = resp
            .json::<OllamaResponse>()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        Ok(ChatCompletionResponse {
            id: format!("ollama-{}", uuid::Uuid::new_v4().simple()),
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp() as u64,
            model: ollama_resp.model,
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant".into(),
                    content: Some(ollama_resp.message.content),
                    tool_calls: None,
                },
                finish_reason: if ollama_resp.done {
                    Some("stop".into())
                } else {
                    None
                },
            }],
            usage: Usage {
                prompt_tokens: ollama_resp.prompt_eval_count,
                completion_tokens: ollama_resp.eval_count,
                total_tokens: ollama_resp.prompt_eval_count + ollama_resp.eval_count,
            },
            system_fingerprint: None,
        })
    }

    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<ChatCompletionChunk, GatewayError>>, GatewayError> {
        info!(model = %request.model, "Ollama streaming request");

        let mut ollama_req = self.convert_request(&request);
        ollama_req.stream = true;

        let url = format!("{}/api/chat", self.base_url);
        let resp = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&ollama_req)
            .send()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::UpstreamError(format!("Ollama {}: {}", status, body)));
        }

        let stream = resp.bytes_stream();
        let model = request.model.clone();
        let id = format!("ollama-{}", uuid::Uuid::new_v4().simple());

        let chunks = stream.filter_map(move |item| {
            let model = model.clone();
            let id = id.clone();
            async move {
                match item {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        for line in text.lines() {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            if let Ok(ollama_resp) = serde_json::from_str::<OllamaResponse>(line) {
                                return Some(Ok(ChatCompletionChunk::new_delta(
                                    &model,
                                    &id,
                                    Some(ollama_resp.message.content),
                                    if ollama_resp.done {
                                        Some("stop".into())
                                    } else {
                                        None
                                    },
                                )));
                            }
                        }
                        None
                    }
                    Err(e) => Some(Err(GatewayError::UpstreamError(e.to_string()))),
                }
            }
        });

        Ok(chunks.boxed())
    }
}
