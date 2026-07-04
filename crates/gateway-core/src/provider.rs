use crate::error::GatewayError;
use crate::types::{ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse};
use async_trait::async_trait;
use futures::stream::BoxStream;

/// Type alias for the streaming response type.
pub type ChunkStream = BoxStream<'static, Result<ChatCompletionChunk, GatewayError>>;

/// Trait that every LLM provider backend must implement.
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Provider name (e.g. "openai", "anthropic").
    fn name(&self) -> &str;

    /// Non-streaming chat completion.
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError>;

    /// Streaming chat completion. Returns a stream of SSE chunks.
    async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChunkStream, GatewayError>;
}
