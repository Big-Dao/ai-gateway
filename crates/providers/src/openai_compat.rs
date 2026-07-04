//! OpenAI-compatible provider adapter.
//!
//! Most modern LLM APIs (DeepSeek, Groq, Together, vLLM, SGLang, TGI, Kimi,
//! Zhipu GLM, etc.) reuse the OpenAI `/v1/chat/completions` wire format. This
//! adapter wraps the production `OpenAIProvider` and applies per-provider
//! behavior tweaks via [`FieldOverrides`] — without duplicating the request /
//! response / SSE plumbing.

pub use gateway_core::config::FieldOverrides;

use std::collections::HashMap;

use crate::openai::OpenAIProvider;

/// An OpenAI-compatible provider.
///
/// Wraps [`OpenAIProvider`] and tags it with a logical name + overrides.
/// The `LLMProvider` impl is delegated directly to the inner OAI provider.
pub struct OpenAICompatProvider {
    name: String,
    inner: OpenAIProvider,
    pub field_overrides: FieldOverrides,
}

impl OpenAICompatProvider {
    pub fn new(
        name: impl Into<String>,
        api_key: Option<String>,
        base_url: impl Into<String>,
        _models: Vec<String>,
        extra_headers: HashMap<String, String>,
        field_overrides: FieldOverrides,
    ) -> Self {
        Self {
            name: name.into(),
            inner: OpenAIProvider::new(api_key, Some(base_url.into()), extra_headers),
            field_overrides,
        }
    }
}

// Implement LLMProvider by delegation.
#[async_trait::async_trait]
impl gateway_core::provider::LLMProvider for OpenAICompatProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chat_completion(
        &self,
        mut request: gateway_core::types::ChatCompletionRequest,
    ) -> Result<gateway_core::types::ChatCompletionResponse, gateway_core::error::GatewayError>
    {
        apply_field_overrides(&self.field_overrides, &mut request);
        self.inner.chat_completion(request).await
    }

    async fn chat_completion_stream(
        &self,
        mut request: gateway_core::types::ChatCompletionRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<gateway_core::types::ChatCompletionChunk, gateway_core::error::GatewayError>,
        >,
        gateway_core::error::GatewayError,
    > {
        apply_field_overrides(&self.field_overrides, &mut request);
        self.inner.chat_completion_stream(request).await
    }
}

/// Apply [`FieldOverrides`] to a request before sending.
fn apply_field_overrides(
    overrides: &FieldOverrides,
    request: &mut gateway_core::types::ChatCompletionRequest,
) {
    if overrides.emit_reasoning_content {
        // Marker so future handlers know to surface reasoning_content.
        request
            .extra
            .insert("emit_reasoning_content".into(), serde_json::json!(true));
    }
    if let Some(kwargs) = &overrides.chat_template_kwargs {
        request
            .extra
            .insert("chat_template_kwargs".into(), kwargs.clone());
    }
    if !overrides.stream_field_renames.is_empty() {
        request.extra.insert(
            "stream_field_renames".into(),
            serde_json::to_value(&overrides.stream_field_renames).unwrap_or_default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_overrides_default() {
        let o = FieldOverrides::default();
        assert!(!o.emit_reasoning_content);
        assert!(o.chat_template_kwargs.is_none());
        assert!(o.stream_field_renames.is_empty());
    }

    #[test]
    fn test_field_overrides_serde() {
        let json = r#"{
            "emit_reasoning_content": true,
            "chat_template_kwargs": {"thinking": {"type": "enabled"}},
            "stream_field_renames": {"delta": "content"}
        }"#;
        let o: FieldOverrides = serde_json::from_str(json).unwrap();
        assert!(o.emit_reasoning_content);
        assert!(o.chat_template_kwargs.is_some());
        assert_eq!(o.stream_field_renames["delta"], "content");
    }

    #[test]
    fn test_compat_provider_name() {
        use gateway_core::provider::LLMProvider;
        let p = OpenAICompatProvider::new(
            "deepseek",
            Some("test-key".into()),
            "https://api.deepseek.com/v1",
            vec!["deepseek-chat".into()],
            HashMap::new(),
            FieldOverrides::default(),
        );
        assert_eq!(p.name(), "deepseek");
    }

    #[test]
    fn test_apply_overrides() {
        let overrides = FieldOverrides {
            emit_reasoning_content: true,
            chat_template_kwargs: Some(serde_json::json!({"thinking": true})),
            stream_field_renames: HashMap::new(),
        };
        let mut req = gateway_core::types::ChatCompletionRequest {
            model: "m".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stream: false,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            user: None,
            response_format: None,
            extra: HashMap::new(),
        };
        apply_field_overrides(&overrides, &mut req);
        assert_eq!(req.extra["emit_reasoning_content"], serde_json::json!(true));
        assert!(req.extra.contains_key("chat_template_kwargs"));
    }
}
