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

#[derive(Serialize)]
struct GeminiRequest<'a> {
    contents: Vec<GeminiContent<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiSystemInstruction<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Serialize)]
struct GeminiContent<'a> {
    role: &'a str,
    parts: Vec<GeminiPart<'a>>,
}

#[derive(Serialize)]
struct GeminiPart<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct GeminiSystemInstruction<'a> {
    parts: Vec<GeminiPart<'a>>,
}

#[derive(Serialize)]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiCandidateContent,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct GeminiCandidateContent {
    parts: Vec<GeminiResponsePart>,
    role: Option<String>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    text: Option<String>,
}

#[derive(Deserialize)]
struct GeminiUsage {
    prompt_token_count: u32,
    candidates_token_count: u32,
    total_token_count: u32,
}

pub struct GeminiProvider {
    name: String,
    client: Client,
    api_key: Option<String>,
    base_url: String,
    extra_headers: HashMap<String, String>,
}

impl GeminiProvider {
    pub fn new(
        api_key: Option<String>,
        base_url: Option<String>,
        extra_headers: HashMap<String, String>,
    ) -> Self {
        Self {
            name: "gemini".into(),
            client: Client::new(),
            api_key,
            base_url: base_url
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".into()),
            extra_headers,
        }
    }

    /// Build a HeaderMap from `extra_headers`. Gemini authenticates via the
    /// `key` query param (applied separately in each request), so this only
    /// carries the user-supplied headers.
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

    fn convert_request<'a>(&self, req: &'a ChatCompletionRequest) -> GeminiRequest<'a> {
        let mut system_instruction = None;
        let mut contents = Vec::new();

        for msg in &req.messages {
            let role = match msg.role.as_str() {
                "system" => {
                    if let Some(Content::Text(ref text)) = msg.content {
                        system_instruction = Some(GeminiSystemInstruction {
                            parts: vec![GeminiPart { text }],
                        });
                    }
                    continue;
                }
                "assistant" => "model",
                _ => "user",
            };

            let text = match &msg.content {
                Some(Content::Text(t)) => t.as_str(),
                Some(Content::Parts(parts)) => {
                    parts.first().and_then(|p| p.text.as_deref()).unwrap_or("")
                }
                None => "",
            };

            contents.push(GeminiContent {
                role,
                parts: vec![GeminiPart { text }],
            });
        }

        GeminiRequest {
            contents,
            system_instruction,
            generation_config: Some(GeminiGenerationConfig {
                temperature: req.temperature,
                max_output_tokens: req.max_tokens,
                top_p: req.top_p,
            }),
        }
    }

    fn convert_response(&self, resp: GeminiResponse, model: &str) -> ChatCompletionResponse {
        let content = resp
            .candidates
            .first()
            .map(|c| {
                c.content
                    .parts
                    .iter()
                    .filter_map(|p| p.text.clone())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        let finish_reason = resp
            .candidates
            .first()
            .and_then(|c| c.finish_reason.clone())
            .map(|r| match r.as_str() {
                "STOP" => "stop".into(),
                "MAX_TOKENS" => "length".into(),
                other => other.to_lowercase(),
            });

        let usage = resp
            .usage_metadata
            .map(|u| Usage {
                prompt_tokens: u.prompt_token_count,
                completion_tokens: u.candidates_token_count,
                total_tokens: u.total_token_count,
            })
            .unwrap_or_default();

        ChatCompletionResponse {
            id: format!("gemini-{}", uuid::Uuid::new_v4().simple()),
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp() as u64,
            model: model.into(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant".into(),
                    content: Some(content),
                    tool_calls: None,
                },
                finish_reason,
            }],
            usage,
            system_fingerprint: None,
        }
    }
}

#[async_trait]
impl LLMProvider for GeminiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        info!(model = %request.model, "Gemini non-streaming request");

        let gemini_req = self.convert_request(&request);
        let url = format!("{}/models/{}:generateContent", self.base_url, request.model);
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&gemini_req);
        if let Some(key) = &self.api_key {
            req = req.query(&[("key", key)]);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(%status, %body, "Gemini upstream error");
            return Err(GatewayError::UpstreamError(format!(
                "Gemini {}: {}",
                status, body
            )));
        }

        let gemini_resp = resp
            .json::<GeminiResponse>()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        Ok(self.convert_response(gemini_resp, &request.model))
    }

    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<ChatCompletionChunk, GatewayError>>,
        GatewayError,
    > {
        info!(model = %request.model, "Gemini streaming request");

        let gemini_req = self.convert_request(&request);
        let url = format!(
            "{}/models/{}:streamGenerateContent",
            self.base_url, request.model
        );
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .query(&[("alt", "sse")])
            .json(&gemini_req);

        if let Some(key) = &self.api_key {
            req = req.query(&[("key", key)]);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| GatewayError::UpstreamError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::UpstreamError(format!(
                "Gemini {}: {}",
                status, body
            )));
        }

        let stream = resp.bytes_stream();
        let model = request.model.clone();
        let id = format!("gemini-{}", uuid::Uuid::new_v4().simple());

        let chunks = stream.filter_map(move |item| {
            let model = model.clone();
            let id = id.clone();
            async move {
                match item {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        for line in text.lines() {
                            let line = line.trim();
                            if line.starts_with("data: ") {
                                let data = &line[6..];
                                if let Ok(gemini_resp) =
                                    serde_json::from_str::<GeminiResponse>(data)
                                {
                                    if let Some(candidate) = gemini_resp.candidates.first() {
                                        let content: String = candidate
                                            .content
                                            .parts
                                            .iter()
                                            .filter_map(|p| p.text.clone())
                                            .collect();
                                        let finish =
                                            candidate.finish_reason.clone().map(|r| {
                                                match r.as_str() {
                                                    "STOP" => "stop".into(),
                                                    "MAX_TOKENS" => "length".into(),
                                                    other => other.to_lowercase(),
                                                }
                                            });
                                        return Some(Ok(ChatCompletionChunk::new_delta(
                                            &model,
                                            &id,
                                            Some(content),
                                            finish,
                                        )));
                                    }
                                }
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
